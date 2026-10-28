use super::*;
use std::{cell::Cell, time::Instant};

use crate::subscription::TOPIC_SHARDS;
use xlfn_kernel::published_owner::PublishedOwner;

struct InlineBenchmarkSubscription(u64);

// SAFETY: the subscription retains no sink and starts no background activity.
unsafe impl crate::subscription::RtdSubscription for InlineBenchmarkSubscription {
    fn request_cancel(&self) {
        std::hint::black_box(self.0);
    }

    fn disconnect_and_wait(self) -> crate::XllResult<()> {
        std::hint::black_box(self.0);
        Ok(())
    }
}

struct InlineBenchmarkSource;

// SAFETY: subscribe discards its sink immediately and the source owns no workers.
unsafe impl crate::subscription::RtdSource for InlineBenchmarkSource {
    type Value = i32;
    type Subscription = InlineBenchmarkSubscription;

    fn subscribe(
        &self,
        _topic: &crate::subscription::RtdTopic,
        _sink: crate::subscription::RtdSink<Self::Value>,
    ) -> crate::XllResult<Self::Subscription> {
        Ok(InlineBenchmarkSubscription(1))
    }
}

/// Runs a measurement with fixture construction, preparation and driver storage
/// outside its scope. Every invocation connects `count` subscriptions before
/// disconnecting any of them, so it exercises the source's live high-water mark.
///
/// Adapter mode uses the actual registered-source adapter with a nonallocating,
/// nonzero-sized subscription. Runtime mode additionally includes connection
/// publication, catalog transitions and synchronous server disconnection. An
/// outstanding preparation guard preserves each canonical topic between runs.
/// `warm` recycles a complete batch before passing the operation to `measure`.
pub fn with_rtd_connection_benchmark<R>(
    count: usize,
    adapter_only: bool,
    warm: bool,
    measure: impl FnOnce(&mut dyn FnMut()) -> R,
) -> R {
    with_rtd_connection_source::<_, InlineBenchmarkSubscription, _>(
        InlineBenchmarkSource,
        count,
        adapter_only,
        warm,
        4096,
        measure,
    )
}

struct PaddedBenchmarkSubscription<const N: usize>([u8; N]);

// SAFETY: this size probe owns no sink and starts no work.
unsafe impl<const N: usize> crate::RtdSubscription for PaddedBenchmarkSubscription<N> {
    fn request_cancel(&self) {
        std::hint::black_box(&self.0);
    }

    fn disconnect_and_wait(self) -> crate::XllResult<()> {
        std::hint::black_box(self.0);
        Ok(())
    }
}

crate::rtd_subscriptions! {
    #[allow(dead_code, clippy::large_enum_variant, reason = "the unused large variant measures closed-enum storage padding")]
    enum BenchmarkOwners256 {
        Small(InlineBenchmarkSubscription),
        Large(PaddedBenchmarkSubscription<256>),
    }
}

crate::rtd_subscriptions! {
    #[allow(dead_code, clippy::large_enum_variant, reason = "the unused large variant measures closed-enum storage padding")]
    enum BenchmarkOwners4096 {
        Small(InlineBenchmarkSubscription),
        Large(PaddedBenchmarkSubscription<4096>),
    }
}

/// Measures an eight-byte live subscription with configurable cleanup capacity
/// and optional rare large variants in the common subscription enum. Layout
/// arguments report the common enum and its affine by-value owner's sizes.
pub fn with_rtd_connection_memory_benchmark<R>(
    count: usize,
    max_active: usize,
    owner_payload_target: usize,
    measure: impl FnOnce(&mut dyn FnMut(), usize, usize) -> R,
) -> R {
    fn run<E, R>(
        count: usize,
        max_active: usize,
        measure: impl FnOnce(&mut dyn FnMut(), usize, usize) -> R,
    ) -> R
    where
        E: crate::RtdSubscription,
        InlineBenchmarkSubscription: crate::IntoRtdSubscription<E>,
    {
        with_rtd_connection_source::<_, E, _>(
            InlineBenchmarkSource,
            count,
            false,
            false,
            max_active,
            |operation| {
                measure(
                    operation,
                    std::mem::size_of::<E>(),
                    std::mem::size_of::<crate::subscription::SourceSubscription<E>>(),
                )
            },
        )
    }
    match owner_payload_target {
        8 => run::<InlineBenchmarkSubscription, _>(count, max_active, measure),
        256 => run::<BenchmarkOwners256, _>(count, max_active, measure),
        4096 => run::<BenchmarkOwners4096, _>(count, max_active, measure),
        _ => panic!("unsupported common subscription payload size"),
    }
}

fn with_rtd_connection_source<S, E, R>(
    source: S,
    count: usize,
    adapter_only: bool,
    warm: bool,
    max_active: usize,
    measure: impl FnOnce(&mut dyn FnMut()) -> R,
) -> R
where
    S: crate::subscription::RtdSource,
    E: crate::RtdSubscription,
    S::Subscription: crate::IntoRtdSubscription<E>,
{
    use crate::subscription::SubscriptionHost;

    assert!((1..=4096).contains(&count));
    assert!(count <= max_active);
    assert_ne!(std::mem::size_of::<InlineBenchmarkSubscription>(), 0);
    let generation = crate::generation::RuntimeGeneration::new(1).unwrap();
    let registration = crate::subscription::SourceRegistration::<E>::new(generation);
    let source_handle = registration.register(source).unwrap();
    let limits = crate::subscription::RtdLimits::standard().with_max_active(
        crate::subscription::RtdCapacity::disabled_if_zero(max_active),
    );
    let runtime = PublishedOwner::new(crate::subscription::SubscriptionRuntime::<E>::with_host(
        generation,
        limits,
        crate::excel_rtd::RtdSubscriptionHost::default(),
        registration.finish(),
    ));
    // SAFETY: PublishedOwner retains this allocation across owner moves and reclaims it only
    // after the callback, all subscriptions and the server handle are dropped.
    let server_handle = unsafe {
        runtime
            .register_server(crate::subscription::ServerGeneration::new(1).unwrap())
            .unwrap()
    };
    let topics: Vec<_> = (0..count)
        .map(|index| crate::subscription::RtdTopic::single(format!("connection-{index}")).unwrap())
        .collect();

    if adapter_only {
        let source = runtime.sources.resolve(source_handle.id).unwrap();
        let server = runtime
            .resolve_server(crate::subscription::ServerGeneration::new(1).unwrap())
            .unwrap();
        let sink = crate::excel_rtd::RtdSubscriptionHost::publish_sink(
            server.publish.as_ref(),
            crate::subscription::TopicId(1),
            crate::generation::ConnectionGeneration::new(1).unwrap(),
        );
        let mut subscriptions = Vec::with_capacity(count);
        let mut operation = || {
            for topic in &topics {
                // SAFETY: this scope owns the stable runtime/source arena and
                // publish core until every ticket has finished or been dropped.
                subscriptions.push(unsafe {
                    crate::subscription::SourceSubscription::subscribe(&source, topic, sink)
                        .unwrap()
                });
            }
            for mut subscription in subscriptions.drain(..) {
                subscription.request_cancel();
                subscription.disconnect_and_wait().unwrap();
            }
        };
        if warm {
            operation();
        }
        measure(&mut operation)
    } else {
        let prepared: Vec<_> = topics
            .iter()
            .map(|topic| runtime.prepare(&source_handle, topic.borrowed()).unwrap())
            .collect();
        let mut operation = || {
            for (index, prepared) in prepared.iter().enumerate() {
                runtime
                    .connect_transaction(
                        &server_handle,
                        crate::subscription::TopicId(index as i32 + 1),
                        prepared.id(),
                    )
                    .unwrap()
                    .commit()
                    .unwrap();
            }
            for index in 0..count {
                server_handle
                    .disconnect(crate::subscription::TopicId(index as i32 + 1))
                    .unwrap();
            }
        };
        if warm {
            operation();
        }
        measure(&mut operation)
    }
}

struct BenchmarkSubscription;

// SAFETY: the benchmark subscription owns no background activity or retained
// sink clones, so disconnect returns only after all possible publication ends.
unsafe impl crate::subscription::RtdSubscription for BenchmarkSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self) -> crate::XllResult<()> {
        Ok(())
    }
}

struct BenchmarkRtdSource<T> {
    sink: Arc<crate::sync::Mutex<Option<crate::subscription::RtdSink<T>>>>,
}

// SAFETY: the benchmark source stores its only retained sink in the shared
// slot, and the benchmark does not use it after its runtime is dropped.
unsafe impl<T: crate::subscription::IntoRtdValue + Clone + Send + Sync + 'static>
    crate::subscription::RtdSource for BenchmarkRtdSource<T>
{
    type Value = T;
    type Subscription = BenchmarkSubscription;

    fn subscribe(
        &self,
        _topic: &crate::subscription::RtdTopic,
        sink: crate::subscription::RtdSink<Self::Value>,
    ) -> crate::XllResult<Self::Subscription> {
        *self.sink.lock() = Some(sink);
        Ok(BenchmarkSubscription)
    }
}

/// A batch of prebuilt topics; construction and runtime teardown stay outside timing.
pub struct RtdPrepareBenchmark {
    runtime: crate::subscription::SubscriptionRuntime<BenchmarkSubscription>,
    source: crate::subscription::RtdSourceHandle<BenchmarkRtdSource<f64>>,
    topics: Vec<crate::subscription::RtdTopic>,
}

impl RtdPrepareBenchmark {
    /// Admission benchmark with inputs and quota occupancy outside measurement.
    /// Inputs and initial quota occupancy are prepared outside measurement.
    pub fn new_admission(case: &str, topic_bytes: usize) -> Self {
        let mut benchmark = Self::new(0, 1, false);
        assert!(topic_bytes > 0 && topic_bytes <= 1024 * 1024);
        let mut remaining = topic_bytes;
        let mut parts = Vec::new();
        while remaining != 0 {
            let length = remaining.min(16 * 1024);
            parts.push("x".repeat(length));
            remaining -= length;
        }
        let topic = crate::subscription::RtdTopic::new(&parts).expect("valid probe topic");
        benchmark.runtime.limits = match case {
            "pending_disabled" => crate::subscription::RtdLimits::standard()
                .with_max_pending(crate::subscription::RtdCapacity::disabled()),
            "pending_full" | "existing_reuse" => crate::subscription::RtdLimits::standard()
                .with_max_pending(crate::subscription::RtdCapacity::disabled_if_zero(1)),
            "source_disabled" => crate::subscription::RtdLimits::standard()
                .with_max_source_ids(crate::subscription::RtdCapacity::disabled()),
            "admitted" => crate::subscription::RtdLimits::standard(),
            "byte_budget" => crate::subscription::RtdLimits::standard().with_max_total_topic_bytes(
                crate::subscription::RtdCapacity::disabled_if_zero(topic_bytes - 1),
            ),
            _ => panic!("unknown quota probe case"),
        };
        if case == "pending_full" {
            let seed = crate::subscription::RtdTopic::single("seed").expect("valid seed topic");
            benchmark
                .runtime
                .prepare(&benchmark.source, seed.borrowed())
                .expect("fill pending quota")
                .commit();
        } else if case == "existing_reuse" {
            benchmark
                .runtime
                .prepare(&benchmark.source, topic.borrowed())
                .expect("seed reusable topic")
                .commit();
        }
        benchmark.topics.push(topic);
        benchmark
    }

    /// Measures only runtime preparation of a prevalidated borrowed topic.
    pub fn run_admission(&self, expect_overloaded: bool) {
        let topic = &self.topics[0];
        match self.runtime.prepare(&self.source, topic.borrowed()) {
            Ok(prepared) => {
                assert!(
                    !expect_overloaded,
                    "quota probe unexpectedly admitted topic"
                );
                std::hint::black_box(prepared.id());
                prepared.rollback();
            }
            Err(crate::XllError::Overloaded) => {
                assert!(expect_overloaded, "reusable topic was rejected");
            }
            Err(error) => panic!("unexpected probe error: {error}"),
        }
    }

    pub fn new(count: usize, parts: usize, existing: bool) -> Self {
        let generation =
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let source = registration
            .register(BenchmarkRtdSource::<f64> {
                sink: Arc::new(crate::sync::Mutex::new(None)),
            })
            .expect("benchmark source registration");
        let runtime = crate::subscription::SubscriptionRuntime::with_sources_for_internal(
            registration.finish(),
        );
        let topics: Vec<_> = (0..count)
            .map(|index| {
                crate::subscription::RtdTopic::new(
                    (0..parts).map(|part| format!("market-{index:06}-field-{part:02}")),
                )
                .expect("valid benchmark topic")
            })
            .collect();
        if existing {
            for topic in &topics {
                runtime
                    .prepare(&source, topic.borrowed())
                    .expect("seed existing topic")
                    .commit();
            }
        }
        Self {
            runtime,
            source,
            topics,
        }
    }

    /// Prepare and commit each prebuilt topic, either new or already present.
    pub fn run_prepare(&mut self) {
        for topic in self.topics.drain(..) {
            let prepared = self
                .runtime
                .prepare(&self.source, topic.borrowed())
                .expect("prepare topic");
            std::hint::black_box(prepared.id());
            prepared.commit();
        }
    }

    /// Grow the catalog with the entire batch, then remove every reservation.
    pub fn run_churn(&mut self) {
        let prepared: Vec<_> = self
            .topics
            .drain(..)
            .map(|topic| {
                self.runtime
                    .prepare(&self.source, topic.borrowed())
                    .expect("prepare topic")
            })
            .collect();
        for reservation in prepared {
            std::hint::black_box(reservation.id());
            reservation.rollback();
        }
    }

    /// Include input validation, hashing, and canonical topic materialization.
    pub fn run_subscribe_input(&self, churn: bool) {
        let prepare = |topic: &crate::subscription::RtdTopic| {
            let parts: smallvec::SmallVec<[&str; 16]> = topic.parts().collect();
            self.runtime
                .prepare(
                    &self.source,
                    crate::subscription::BorrowedTopicParts::new(&parts)
                        .expect("valid input parts"),
                )
                .expect("prepare input")
        };
        if churn {
            let prepared: Vec<_> = self.topics.iter().map(prepare).collect();
            for reservation in prepared {
                std::hint::black_box(reservation.id());
                reservation.rollback();
            }
        } else {
            for topic in &self.topics {
                let prepared = prepare(topic);
                std::hint::black_box(prepared.id());
                prepared.commit();
            }
        }
    }
}

pub struct RtdPublishNumberBenchmark {
    _runtime: PublishedOwner<crate::subscription::SubscriptionRuntime<BenchmarkSubscription>>,
    _server: crate::subscription::SubscriptionServerHandle<BenchmarkSubscription>,
    sink: crate::subscription::RtdSink<f64>,
}

impl Default for RtdPublishNumberBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishNumberBenchmark {
    pub fn new() -> Self {
        let generation =
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let sink_slot = Arc::new(crate::sync::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = PublishedOwner::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: PublishedOwner retains the runtime across fixture moves; all handle
        // uses end before runtime teardown.
        let server = unsafe {
            runtime
                .register_server(
                    crate::subscription::ServerGeneration::new(1)
                        .expect("non-zero test server generation"),
                )
                .expect("server registration must succeed")
        };
        let topic = crate::subscription::RtdTopic::new(["BENCH", "NUMBER"])
            .expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(&source, topic.borrowed())
            .expect("prepare must succeed");
        let id = prepared.id();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(1), id)
            .expect("connect_transaction must succeed");
        conn.commit().expect("connection commit must succeed");
        prepared.commit();
        let sink = sink_slot.lock().clone().expect("sink must be captured");
        Self {
            _runtime: runtime,
            _server: server,
            sink,
        }
    }

    #[inline]
    pub fn run_repeated_same(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink.publish(12.5).expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_changing(&self, iterations: usize) {
        for i in 0..iterations {
            self.sink
                .publish(12.5 + i as f64)
                .expect("publish must succeed");
        }
    }
}

pub struct RtdPublishStringBenchmark {
    _runtime: PublishedOwner<crate::subscription::SubscriptionRuntime<BenchmarkSubscription>>,
    _server: crate::subscription::SubscriptionServerHandle<BenchmarkSubscription>,
    sink: crate::subscription::RtdSink<String>,
    first_value: String,
    second_value: String,
}

impl Default for RtdPublishStringBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishStringBenchmark {
    pub fn new() -> Self {
        Self::with_values(
            "stream_market_data_update_payload_a".to_owned(),
            "stream_market_data_update_payload_b".to_owned(),
        )
    }

    pub fn with_payload_len(payload_len: usize) -> Self {
        assert!(payload_len > 0, "benchmark payload length must be non-zero");
        Self::with_values(
            benchmark_string_payload(payload_len, b'a'),
            benchmark_string_payload(payload_len, b'b'),
        )
    }

    fn with_values(first_value: String, second_value: String) -> Self {
        let generation =
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let sink_slot = Arc::new(crate::sync::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = PublishedOwner::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: PublishedOwner retains the runtime across fixture moves; all handle
        // uses end before runtime teardown.
        let server = unsafe {
            runtime
                .register_server(
                    crate::subscription::ServerGeneration::new(1)
                        .expect("non-zero test server generation"),
                )
                .expect("server registration must succeed")
        };
        let topic = crate::subscription::RtdTopic::new(["BENCH", "STRING"])
            .expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(&source, topic.borrowed())
            .expect("prepare must succeed");
        let id = prepared.id();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(2), id)
            .expect("connect_transaction must succeed");
        conn.commit().expect("connection commit must succeed");
        prepared.commit();
        let sink = sink_slot.lock().clone().expect("sink must be captured");
        Self {
            _runtime: runtime,
            _server: server,
            sink,
            first_value,
            second_value,
        }
    }

    #[inline]
    pub fn run_repeated_same(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish(self.first_value.clone())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_changing(&self, iterations: usize) {
        // Alternate fixed payloads so this measures changing-value work
        // without adding formatting overhead to the workload.
        for index in 0..iterations {
            let value = if index & 1 == 0 {
                &self.first_value
            } else {
                &self.second_value
            };
            self.sink
                .publish(String::clone(value))
                .expect("publish must succeed");
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RtdRefreshValueKind {
    Number,
    ShortString,
    String1KiB,
    String8KiB,
}

#[derive(Clone, Copy, Debug)]
pub struct RtdRefreshScalingCase {
    pub name: &'static str,
    pub active_topics: usize,
    pub updated_topics: usize,
    pub ready_shards: usize,
}

pub const RTD_REFRESH_SCALING_CASES: [RtdRefreshScalingCase; 3] = [
    RtdRefreshScalingCase {
        name: "sparse",
        active_topics: 4_096,
        updated_topics: 1,
        ready_shards: 1,
    },
    RtdRefreshScalingCase {
        name: "medium",
        active_topics: 4_096,
        updated_topics: 128,
        ready_shards: 4,
    },
    RtdRefreshScalingCase {
        name: "dense",
        active_topics: 4_096,
        updated_topics: 4_096,
        ready_shards: 32,
    },
];

enum RtdRefreshSinks {
    Number(Vec<crate::subscription::RtdSink<f64>>),
    String {
        sinks: Vec<crate::subscription::RtdSink<String>>,
        first_value: String,
        second_value: String,
    },
}

fn string_refresh_sinks(
    topic_ids: &[crate::subscription::TopicId],
    first_value: String,
    second_value: String,
) -> (
    PublishedOwner<crate::subscription::SubscriptionRuntime<BenchmarkSubscription>>,
    crate::subscription::SubscriptionServerHandle<BenchmarkSubscription>,
    RtdRefreshSinks,
) {
    let (runtime, server, sinks) = build_refresh_topics::<String>(topic_ids);
    (
        runtime,
        server,
        RtdRefreshSinks::String {
            sinks,
            first_value,
            second_value,
        },
    )
}

pub struct RtdRefreshScalingBenchmark {
    _runtime: PublishedOwner<crate::subscription::SubscriptionRuntime<BenchmarkSubscription>>,
    server: crate::subscription::SubscriptionServerHandle<BenchmarkSubscription>,
    sinks: RtdRefreshSinks,
    updated_indices: Vec<usize>,
    publish_revision: Cell<u64>,
}

impl RtdRefreshScalingBenchmark {
    /// Models an initial full subscription snapshot followed by sparse ticks.
    /// Both epoch buffers retain their peak storage before measurement starts.
    pub fn after_dense_refresh(
        case: RtdRefreshScalingCase,
        value_kind: RtdRefreshValueKind,
    ) -> Self {
        let mut benchmark = Self::new(case, value_kind);
        benchmark.updated_indices = (0..case.active_topics).collect();
        benchmark.run_end_to_end_cycle();
        benchmark.run_end_to_end_cycle();
        // Update late topic IDs, not the initial snapshot's first key. A
        // hash-table iterator may stop early for a low occupied bucket and
        // accidentally hide historical-capacity scans in that special case.
        benchmark.updated_indices =
            (case.active_topics - case.updated_topics..case.active_topics).collect();
        benchmark
    }

    pub fn new(case: RtdRefreshScalingCase, value_kind: RtdRefreshValueKind) -> Self {
        assert!(case.active_topics > 0);
        assert!(case.updated_topics > 0);
        assert!(case.updated_topics <= case.active_topics);
        assert!((1..=TOPIC_SHARDS).contains(&case.ready_shards));

        let topic_ids = topic_ids_for_case(case);

        let (runtime, server, sinks) = match value_kind {
            RtdRefreshValueKind::Number => {
                let (runtime, server, sinks) = build_refresh_topics::<f64>(&topic_ids);
                (runtime, server, RtdRefreshSinks::Number(sinks))
            }
            RtdRefreshValueKind::ShortString => string_refresh_sinks(
                &topic_ids,
                "market-update-a".to_owned(),
                "market-update-b".to_owned(),
            ),
            RtdRefreshValueKind::String1KiB => string_refresh_sinks(
                &topic_ids,
                benchmark_string_payload(1024, b'a'),
                benchmark_string_payload(1024, b'b'),
            ),
            RtdRefreshValueKind::String8KiB => string_refresh_sinks(
                &topic_ids,
                benchmark_string_payload(8 * 1024, b'a'),
                benchmark_string_payload(8 * 1024, b'b'),
            ),
        };
        let updated_indices = (0..case.updated_topics).collect();

        Self {
            _runtime: runtime,
            server,
            sinks,
            updated_indices,
            publish_revision: Cell::new(0),
        }
    }

    fn drain_pending_refresh(&self) {
        if let Ok(batch) = self.server.begin_refresh() {
            let _ = batch.complete(crate::subscription::RefreshOutcome::Delivered);
        }
    }

    pub fn measure_refresh_collection(&self, iterations: u64) -> Duration {
        self.drain_pending_refresh();
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let mut planned = self
                .server
                .server()
                .expect("benchmark server remains registered")
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            let started = Instant::now();
            planned
                .publish
                .collect_refresh(&planned.plan, &mut planned.updates);
            measured += started.elapsed();
            let updates = std::mem::take(&mut planned.updates);
            planned
                .publish
                .restore_refresh_updates(&planned.plan, updates);
            planned.finished = true;
            drop(planned);
        }
        self.drain_pending_refresh();
        measured
    }

    pub fn measure_refresh_completion(&self, iterations: u64) -> Duration {
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            self.publish_updates();
            let batch = self
                .server
                .begin_refresh()
                .expect("begin_refresh must succeed");
            let started = Instant::now();
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .expect("refresh completion must succeed");
            measured += started.elapsed();
        }
        measured
    }

    #[inline]
    pub fn run_end_to_end_cycle(&self) {
        self.publish_updates();
        let batch = self
            .server
            .begin_refresh()
            .expect("begin_refresh must succeed");
        batch
            .complete(crate::subscription::RefreshOutcome::Delivered)
            .expect("refresh completion must succeed");
    }

    /// Retains the runtime and its terminal server handle for footprint probes.
    fn publish_updates(&self) {
        let revision = self.publish_revision.get();
        self.publish_revision.set(revision.wrapping_add(1));
        match &self.sinks {
            RtdRefreshSinks::Number(sinks) => {
                let value = if revision & 1 == 0 { 12.5 } else { 13.5 };
                for &index in &self.updated_indices {
                    sinks[index]
                        .publish(value)
                        .expect("number publish must succeed");
                }
            }
            RtdRefreshSinks::String {
                sinks,
                first_value,
                second_value,
                ..
            } => {
                let value = if revision & 1 == 0 {
                    first_value
                } else {
                    second_value
                };
                for &index in &self.updated_indices {
                    sinks[index]
                        .publish(value.as_str().to_owned())
                        .expect("string publish must succeed");
                }
            }
        }
    }
}

fn benchmark_string_payload(payload_len: usize, suffix: u8) -> String {
    debug_assert!(matches!(suffix, b'a' | b'b'));
    let mut bytes = vec![b'x'; payload_len];
    bytes[payload_len - 1] = suffix;
    String::from_utf8(bytes).expect("benchmark payload is valid ASCII")
}

fn build_refresh_topics<T>(
    topic_ids: &[crate::subscription::TopicId],
) -> (
    PublishedOwner<crate::subscription::SubscriptionRuntime<BenchmarkSubscription>>,
    crate::subscription::SubscriptionServerHandle<BenchmarkSubscription>,
    Vec<crate::subscription::RtdSink<T>>,
)
where
    T: crate::subscription::IntoRtdValue + Clone + Send + Sync + 'static,
{
    let generation =
        crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero");
    let registration = crate::subscription::SourceRegistration::new(generation);
    let registered = topic_ids
        .iter()
        .map(|_| {
            let sink_slot = Arc::new(crate::sync::Mutex::new(None));
            let source = registration
                .register(BenchmarkRtdSource {
                    sink: Arc::clone(&sink_slot),
                })
                .expect("benchmark source handle allocation must succeed");
            (source, sink_slot)
        })
        .collect::<Vec<_>>();
    let runtime = PublishedOwner::new(
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish()),
    );
    // SAFETY: PublishedOwner retains the runtime across fixture moves; all handle
    // uses end before runtime teardown.
    let server = unsafe {
        runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero benchmark server generation"),
            )
            .expect("server registration must succeed")
    };
    let sinks = registered
        .into_iter()
        .zip(topic_ids.iter().copied())
        .enumerate()
        .map(|(index, ((source, sink_slot), topic_id))| {
            let topic = crate::subscription::RtdTopic::single(format!("refresh-{index}"))
                .expect("benchmark RTD topic must be valid");
            let prepared = runtime
                .prepare(&source, topic.borrowed())
                .expect("prepare must succeed");
            let id = prepared.id();
            let connection = runtime
                .connect_transaction(&server, topic_id, id)
                .expect("connect_transaction must succeed");
            connection.commit().expect("connection commit must succeed");
            prepared.commit();
            sink_slot.lock().clone().expect("sink must be captured")
        })
        .collect();
    (runtime, server, sinks)
}

fn topic_ids_for_case(case: RtdRefreshScalingCase) -> Vec<crate::subscription::TopicId> {
    let mut topic_ids = Vec::with_capacity(case.active_topics);
    for ordinal in 0..case.updated_topics {
        let shard = ordinal % case.ready_shards;
        let row = ordinal / case.ready_shards;
        let raw = TOPIC_SHARDS
            .checked_add(row * TOPIC_SHARDS + shard)
            .and_then(|raw| i32::try_from(raw).ok())
            .expect("benchmark topic id must fit i32");
        topic_ids.push(crate::subscription::TopicId(raw));
    }
    let next_raw = topic_ids.last().map_or(TOPIC_SHARDS * 2, |topic_id| {
        topic_id.0 as usize + TOPIC_SHARDS
    });
    for offset in 0..(case.active_topics - case.updated_topics) {
        let raw = i32::try_from(next_raw + offset).expect("benchmark topic id must fit i32");
        topic_ids.push(crate::subscription::TopicId(raw));
    }
    topic_ids
}

/// Persistent real channel adapter, sender workers, PublishCore, and refresh consumer.
/// All worker creation and subscription setup are outside measured cycles.
pub struct RtdChannelPipelineBenchmark {
    _runtime: PublishedOwner<
        crate::subscription::SubscriptionRuntime<crate::subscription::RtdChannelSubscription>,
    >,
    server:
        crate::subscription::SubscriptionServerHandle<crate::subscription::RtdChannelSubscription>,
    sender: crate::subscription::RtdSender<f64>,
    jobs: Vec<std::sync::mpsc::SyncSender<(u64, usize)>>,
    done: std::sync::mpsc::Receiver<usize>,
    workers: Vec<std::thread::JoinHandle<()>>,
    revision: u64,
    last_sequence: Option<u64>,
}

impl RtdChannelPipelineBenchmark {
    pub fn new(capacity: usize, producers: usize) -> Self {
        assert!(producers > 0);
        let registration = crate::subscription::SourceRegistration::new(
            crate::generation::RuntimeGeneration::new(1).unwrap(),
        );
        let (sender_tx, sender_rx) = std::sync::mpsc::sync_channel(1);
        let source = registration
            .register(crate::subscription::RtdChannelSource::new(
                std::num::NonZeroUsize::new(capacity).unwrap(),
                move |_| {
                    let sender_tx = sender_tx.clone();
                    Ok(move |sender: crate::subscription::RtdSender<f64>| {
                        sender_tx.send(sender.clone()).unwrap();
                        while !sender.wait_closed(Duration::from_secs(1)) {}
                        Ok(())
                    })
                },
            ))
            .unwrap();
        let runtime = PublishedOwner::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: PublishedOwner keeps raw handles valid across fixture owner moves.
        let server = unsafe {
            runtime
                .register_server(crate::subscription::ServerGeneration::new(1).unwrap())
                .unwrap()
        };
        let prepared = runtime
            .prepare(
                &source,
                crate::subscription::RtdTopic::single("pipeline")
                    .unwrap()
                    .borrowed(),
            )
            .unwrap();
        runtime
            .connect_transaction(&server, crate::subscription::TopicId(1), prepared.id())
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
        let sender = sender_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let (done_tx, done) = std::sync::mpsc::channel();
        let mut jobs = Vec::new();
        let mut workers = Vec::new();
        for producer in 0..producers {
            let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<(u64, usize)>(1);
            jobs.push(job_tx);
            let sender = sender.clone();
            let done_tx = done_tx.clone();
            workers.push(std::thread::spawn(move || {
                while let Ok((revision, count)) = job_rx.recv() {
                    let mut retries = 0;
                    for index in 0..count {
                        let value = (revision * 100_000_000
                            + producer as u64 * 1_000_000
                            + index as u64) as f64;
                        retries += pipeline_send(&sender, value);
                    }
                    done_tx.send(retries).unwrap();
                }
            }));
        }
        Self {
            _runtime: runtime,
            server,
            sender,
            jobs,
            done,
            workers,
            revision: 0,
            last_sequence: None,
        }
    }

    /// Enqueue all updates, then deliver a FIFO marker through actual refresh
    /// planning and completion. Returns bounded-queue admission retries.
    pub fn run_cycle(&mut self, updates_per_producer: usize) -> usize {
        self.revision += 1;
        for job in &self.jobs {
            job.send((self.revision, updates_per_producer)).unwrap();
        }
        let mut retries = 0;
        for _ in &self.jobs {
            retries += self.done.recv_timeout(Duration::from_secs(30)).unwrap();
        }
        let marker = -(self.revision as f64);
        retries += pipeline_send(&self.sender, marker);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let batch = self.server.begin_refresh().unwrap();
            let final_sequence = batch.updates.iter().find_map(|update| {
                matches!(update.value, crate::subscription::StoredRtdValue::Number(value) if value == marker)
                    .then_some(update.sequence)
            });
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .unwrap();
            if let Some(sequence) = final_sequence {
                if let Some(previous) = self.last_sequence {
                    assert_eq!(
                        sequence - previous,
                        (self.jobs.len() * updates_per_producer + 1) as u64,
                        "every accepted distinct value must reach PublishCore before the marker"
                    );
                }
                self.last_sequence = Some(sequence);
                return retries;
            }
            assert!(
                Instant::now() < deadline,
                "pipeline did not deliver its final marker"
            );
            std::thread::yield_now();
        }
    }
}

fn pipeline_send(sender: &crate::subscription::RtdSender<f64>, value: f64) -> usize {
    let mut retries = 0;
    loop {
        match sender.try_send(value) {
            Ok(()) => return retries,
            Err(crate::rtd::RtdSendError::Full(_)) => {
                retries += 1;
                std::thread::yield_now();
            }
            Err(error) => panic!("pipeline send failed: {error}"),
        }
    }
}

impl Drop for RtdChannelPipelineBenchmark {
    fn drop(&mut self) {
        self.jobs.clear();
        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}

pub fn rtd_pipeline_probe(
    producers: usize,
    capacity: usize,
    operations: usize,
) -> serde_json::Value {
    let mut fixture = RtdChannelPipelineBenchmark::new(capacity, producers);
    let mut elapsed = Vec::new();
    let mut retries = 0;
    fixture.run_cycle(operations);
    for _ in 0..9 {
        let started = Instant::now();
        retries += fixture.run_cycle(operations);
        elapsed.push(started.elapsed().as_nanos() as u64);
    }
    elapsed.sort_unstable();
    serde_json::json!({ "producers": producers, "capacity": capacity,
        "updates_per_producer": operations, "rounds_ns": elapsed,
        "median_ns": elapsed[elapsed.len()/2], "overloaded_retries": retries })
}

#[cfg(test)]
mod published_runtime_tests {
    use super::*;

    fn move_fixture<T>(fixture: T) -> T {
        let mut owners = vec![fixture];
        owners.reserve(4);
        owners.remove(0)
    }

    #[test]
    fn miri_benchmark_published_runtime_owners_survive_moves_and_shutdown() {
        with_rtd_connection_benchmark(2, true, false, |operation| operation());
        with_rtd_connection_benchmark(2, false, false, |operation| operation());

        let number = move_fixture(RtdPublishNumberBenchmark::new());
        number.run_changing(2);
        drop(number);

        let string = move_fixture(RtdPublishStringBenchmark::new());
        string.run_changing(2);
        drop(string);

        for kind in [
            RtdRefreshValueKind::Number,
            RtdRefreshValueKind::ShortString,
        ] {
            let refresh = move_fixture(RtdRefreshScalingBenchmark::new(
                RtdRefreshScalingCase {
                    name: "published-owner-smoke",
                    active_topics: 2,
                    updated_topics: 1,
                    ready_shards: 1,
                },
                kind,
            ));
            refresh.run_end_to_end_cycle();
            drop(refresh);
        }

        let mut pipeline = move_fixture(RtdChannelPipelineBenchmark::new(4, 2));
        pipeline.run_cycle(2);
        drop(pipeline);
    }
}

#[derive(Default)]
struct TopologyCounters {
    started: AtomicUsize,
    finished: AtomicUsize,
    live: AtomicUsize,
    subscribed: AtomicUsize,
    unsubscribed: AtomicUsize,
}

struct TopologyWorkerExit(Arc<TopologyCounters>);

impl TopologyWorkerExit {
    fn start(counters: Arc<TopologyCounters>) -> Self {
        counters.live.fetch_add(1, Ordering::AcqRel);
        counters.started.fetch_add(1, Ordering::Release);
        Self(counters)
    }
}

impl Drop for TopologyWorkerExit {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
        self.0.finished.fetch_add(1, Ordering::Release);
    }
}

/// Measures one source's idle subscription lifecycle through the real RTD
/// runtime, including producer startup and cancellation/join barriers.
///
/// `multiplexed` selects one shared job; otherwise each subscription owns a
/// blocking job. This probe publishes no values and does not call Excel/COM.
/// Worker counts are instrumented producer jobs, excluding infrastructure.
/// Connect and disconnect timings include only synchronous runtime calls;
/// separate readiness/removal waits expose asynchronous completion time.
pub fn rtd_topology_probe(multiplexed: bool, subscriptions: usize) -> serde_json::Value {
    use crate::rtd::{
        RtdCapacity, RtdChannelSource, RtdLatest, RtdMultiplexedSource, RtdSourceEvent,
        RtdSourceLimits,
    };
    use std::num::NonZeroUsize;

    assert!((1..=4096).contains(&subscriptions));
    assert!(multiplexed || subscriptions <= 64);
    let counters = Arc::new(TopologyCounters::default());
    let worker_counters = Arc::clone(&counters);
    let limits = RtdSourceLimits::standard()
        .with_max_subscriptions(RtdCapacity::disabled_if_zero(subscriptions));
    if multiplexed {
        let source = RtdMultiplexedSource::<i32, _, _, _>::new(RtdLatest, move |context| {
            let _exit = TopologyWorkerExit::start(worker_counters);
            loop {
                match context.recv_timeout(Duration::from_secs(1)) {
                    Ok(Some(RtdSourceEvent::Subscribe { .. })) => {
                        _exit.0.subscribed.fetch_add(1, Ordering::Release);
                    }
                    Ok(Some(RtdSourceEvent::Unsubscribe { .. })) => {
                        _exit.0.unsubscribed.fetch_add(1, Ordering::Release);
                    }
                    Ok(None) => {}
                    Err(crate::XllError::Closing) if context.is_closed() => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
        })
        .with_limits(limits);
        run_rtd_topology(source, counters, true, subscriptions)
    } else {
        let source = RtdChannelSource::<i32, _>::new(NonZeroUsize::new(1).unwrap(), move |_| {
            let counters = Arc::clone(&worker_counters);
            Ok(move |sender: crate::rtd::RtdSender<i32>| {
                let _exit = TopologyWorkerExit::start(counters);
                while !sender.wait_closed(Duration::from_secs(1)) {}
                Ok(())
            })
        })
        .with_max_producers(NonZeroUsize::new(subscriptions).unwrap())
        .with_limits(limits);
        run_rtd_topology(source, counters, false, subscriptions)
    }
}

fn topology_wait(counter: &AtomicUsize, expected: usize, what: &str) -> u64 {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    while counter.load(Ordering::Acquire) != expected {
        assert!(
            Instant::now() < deadline,
            "topology probe timed out: {what}"
        );
        std::thread::yield_now();
    }
    started.elapsed().as_nanos() as u64
}

fn run_rtd_topology<S: crate::rtd::RtdSource<Value = i32>>(
    source: S,
    counters: Arc<TopologyCounters>,
    multiplexed: bool,
    subscriptions: usize,
) -> serde_json::Value {
    use crate::rtd::{RtdCapacity, RtdLimits, RtdTopic};

    let generation = crate::generation::RuntimeGeneration::new(1).unwrap();
    let registration = crate::subscription::SourceRegistration::new(generation);
    let source = registration.register(source).unwrap();
    let limits =
        RtdLimits::standard().with_max_active(RtdCapacity::disabled_if_zero(subscriptions));
    let runtime = PublishedOwner::new(crate::subscription::SubscriptionRuntime::with_host(
        generation,
        limits,
        crate::excel_rtd::RtdSubscriptionHost::default(),
        registration.finish(),
    ));
    // SAFETY: the boxed runtime stays at a stable address and outlives the
    // server handle and all operations, including its explicit final close.
    let server = unsafe {
        runtime
            .register_server(crate::subscription::ServerGeneration::new(1).unwrap())
            .unwrap()
    };
    let topics: Vec<_> = (0..subscriptions)
        .map(|index| RtdTopic::single(format!("topology-{index}")).unwrap())
        .collect();

    let started = Instant::now();
    for (index, topic) in topics.iter().enumerate() {
        let prepared = runtime.prepare(&source, topic.borrowed()).unwrap();
        runtime
            .connect_transaction(
                &server,
                crate::subscription::TopicId(index as i32 + 1),
                prepared.id(),
            )
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
    }
    let connect_ns = started.elapsed().as_nanos() as u64;
    let expected_jobs = if multiplexed { 1 } else { subscriptions };
    let start_wait_ns = topology_wait(&counters.started, expected_jobs, "producer startup");
    let subscribe_wait_ns = if multiplexed {
        topology_wait(&counters.subscribed, subscriptions, "subscribe consumption")
    } else {
        0
    };
    assert_eq!(counters.live.load(Ordering::Acquire), expected_jobs);

    let started = Instant::now();
    for index in 0..subscriptions {
        server
            .disconnect(crate::subscription::TopicId(index as i32 + 1))
            .unwrap();
    }
    let disconnect_ns = started.elapsed().as_nanos() as u64;
    let unsubscribe_wait_ns = if multiplexed {
        topology_wait(
            &counters.unsubscribed,
            subscriptions,
            "unsubscribe consumption",
        )
    } else {
        0
    };
    let live_after_disconnect = counters.live.load(Ordering::Acquire);
    assert_eq!(live_after_disconnect, usize::from(multiplexed));

    let started = Instant::now();
    runtime.close().unwrap();
    let close_ns = started.elapsed().as_nanos() as u64;
    assert_eq!(counters.finished.load(Ordering::Acquire), expected_jobs);
    assert_eq!(counters.live.load(Ordering::Acquire), 0);

    serde_json::json!({
        "schema": 1,
        "scope": "local_idle_subscription_lifecycle_without_excel",
        "topology": if multiplexed { "multiplexed" } else { "blocking" },
        "sources": 1,
        "subscriptions": subscriptions,
        "producer_jobs_started": counters.started.load(Ordering::Acquire),
        "producer_jobs_finished": counters.finished.load(Ordering::Acquire),
        "producer_jobs_live_after_disconnect": live_after_disconnect,
        "producer_jobs_live_after_close": counters.live.load(Ordering::Acquire),
        "subscribe_events_consumed": counters.subscribed.load(Ordering::Acquire),
        "unsubscribe_events_consumed": counters.unsubscribed.load(Ordering::Acquire),
        "connect_ns": connect_ns,
        "start_wait_ns": start_wait_ns,
        "subscribe_wait_ns": subscribe_wait_ns,
        "disconnect_ns": disconnect_ns,
        "unsubscribe_wait_ns": unsubscribe_wait_ns,
        "close_ns": close_ns,
    })
}

struct CatalogAllocationSource(u64);
// SAFETY: this allocation probe never retains its sink or starts a worker.
unsafe impl crate::subscription::RtdSource for CatalogAllocationSource {
    type Value = i32;
    type Subscription = ();
    fn subscribe(
        &self,
        _: &crate::subscription::RtdTopic,
        _: crate::subscription::RtdSink<i32>,
    ) -> crate::XllResult<()> {
        std::hint::black_box(self.0);
        Ok(())
    }
}

/// Constructs and releases a source catalog of nonzero-sized, nonallocating
/// source values. Counts 1, 4 and 16 compare static tuple storage with dynamic
/// per-source adapters; no runtime, connection or worker allocations enter.
pub fn rtd_source_catalog_allocation_probe(static_catalog: bool, count: usize) {
    use crate::generation::RuntimeGeneration;
    use crate::subscription::SourceRegistration;
    let generation = RuntimeGeneration::new(1).unwrap();
    if !static_catalog {
        let registration = SourceRegistration::<()>::new(generation);
        for index in 0..count {
            std::hint::black_box(
                registration
                    .register(CatalogAllocationSource(index as u64))
                    .unwrap(),
            );
        }
        std::hint::black_box(registration.finish());
        return;
    }
    macro_rules! fixed_catalog {
        ($($index:literal),+) => {{
            let registration = SourceRegistration::<(),_>::new(generation);
            std::hint::black_box(registration.register_sources(($(CatalogAllocationSource($index),)+)).unwrap());
            std::hint::black_box(registration.finish());
        }};
    }
    match count {
        1 => fixed_catalog!(0),
        4 => fixed_catalog!(0, 1, 2, 3),
        16 => fixed_catalog!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15),
        _ => panic!("catalog allocation probe accepts 1, 4 or 16 sources"),
    }
}
