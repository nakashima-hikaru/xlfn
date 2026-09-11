use super::*;
use std::{cell::Cell, time::Instant};

use crate::subscription::TOPIC_SHARDS;

struct BenchmarkSubscription;

// SAFETY: the benchmark subscription owns no background activity or retained
// sink clones, so disconnect returns only after all possible publication ends.
unsafe impl crate::subscription::RtdSubscription for BenchmarkSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
        Ok(())
    }
}

struct BenchmarkRtdSource<T> {
    sink: Arc<parking_lot::Mutex<Option<crate::subscription::RtdSink<T>>>>,
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
    runtime: crate::subscription::SubscriptionRuntime,
    source: crate::subscription::RtdSourceHandle<BenchmarkRtdSource<f64>>,
    topics: Vec<crate::subscription::RtdTopic>,
}

impl RtdPrepareBenchmark {
    pub fn new(count: usize, parts: usize, existing: bool) -> Self {
        let generation =
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let source = registration
            .register(BenchmarkRtdSource::<f64> {
                sink: Arc::new(parking_lot::Mutex::new(None)),
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
            let parts: smallvec::SmallVec<[&str; 16]> =
                topic.parts().iter().map(|part| part.as_str()).collect();
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
    _runtime: Box<crate::subscription::SubscriptionRuntime>,
    _server: crate::subscription::SubscriptionServerHandle,
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
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = Box::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: `runtime` outlives `server` for the benchmark setup duration.
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
    _runtime: Box<crate::subscription::SubscriptionRuntime>,
    _server: crate::subscription::SubscriptionServerHandle,
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
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = Box::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: `runtime` outlives `server` for the benchmark setup duration.
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
    Box<crate::subscription::SubscriptionRuntime>,
    crate::subscription::SubscriptionServerHandle,
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
    _runtime: Box<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::SubscriptionServerHandle,
    sinks: RtdRefreshSinks,
    updated_indices: Vec<usize>,
    publish_revision: Cell<u64>,
}

impl RtdRefreshScalingBenchmark {
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
            let updates = planned.publish.collect_refresh(&planned.plan);
            measured += started.elapsed();
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
    Box<crate::subscription::SubscriptionRuntime>,
    crate::subscription::SubscriptionServerHandle,
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
            let sink_slot = Arc::new(parking_lot::Mutex::new(None));
            let source = registration
                .register(BenchmarkRtdSource {
                    sink: Arc::clone(&sink_slot),
                })
                .expect("benchmark source handle allocation must succeed");
            (source, sink_slot)
        })
        .collect::<Vec<_>>();
    let runtime = Box::new(
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish()),
    );
    // SAFETY: `runtime` outlives `server` for the benchmark setup duration.
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
    _runtime: Box<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::SubscriptionServerHandle,
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
        let runtime = Box::new(
            crate::subscription::SubscriptionRuntime::with_sources_for_internal(
                registration.finish(),
            ),
        );
        // SAFETY: the boxed runtime stays at a stable address for this fixture.
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
            Err(crate::XllError::Overloaded) => {
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
