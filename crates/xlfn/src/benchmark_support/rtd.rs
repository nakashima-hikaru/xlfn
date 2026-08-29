use super::*;
use rayon::prelude::*;
use std::{cell::Cell, time::Instant};

use crate::subscription::TOPIC_SHARDS;

struct BenchmarkSubscription;

impl crate::subscription::RtdSubscription for BenchmarkSubscription {
    fn cancellation(&self) -> Arc<dyn crate::subscription::RtdCancellation> {
        Arc::new(crate::subscription::RtdCancellationHandle::noop())
    }
    fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
        Ok(())
    }
}

struct BenchmarkRtdSource<T> {
    sink: Arc<parking_lot::Mutex<Option<crate::subscription::RtdSink<T>>>>,
}

impl<T: crate::subscription::IntoRtdValue + Clone + Send + Sync + 'static>
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

pub struct RtdPublishNumberBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::SubscriptionServerHandle,
    sink: crate::subscription::RtdSink<f64>,
}

impl Default for RtdPublishNumberBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishNumberBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .expect("server registration must succeed");
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = crate::subscription::RtdSourceHandle::for_internal(
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero"),
            BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            },
        )
        .expect("benchmark source handle allocation must succeed");
        let topic = crate::subscription::RtdTopic::new(["BENCH", "NUMBER"])
            .expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(&source, topic)
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
            server,
            sink,
        }
    }

    #[inline]
    pub fn run_coalesced(&self, iterations: usize) {
        for i in 0..iterations {
            self.sink
                .publish(12.5 + i as f64)
                .expect("publish must succeed");
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

    #[inline]
    pub fn run_drain_each(&self, iterations: usize) {
        for i in 0..iterations {
            self.sink
                .publish(12.5 + i as f64)
                .expect("publish must succeed");
            let batch = self
                .server
                .begin_refresh()
                .expect("begin_refresh must succeed");
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .expect("complete must succeed");
        }
    }
}

pub struct RtdPublishStringBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::SubscriptionServerHandle,
    sink: crate::subscription::RtdSink<String>,
}

impl Default for RtdPublishStringBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishStringBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .expect("server registration must succeed");
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = crate::subscription::RtdSourceHandle::for_internal(
            crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero"),
            BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            },
        )
        .expect("benchmark source handle allocation must succeed");
        let topic = crate::subscription::RtdTopic::new(["BENCH", "STRING"])
            .expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(&source, topic)
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
            server,
            sink,
        }
    }

    #[inline]
    pub fn run_coalesced(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_repeated_same(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_changing(&self, iterations: usize) {
        // Alternate fixed payloads so this measures changing-value work
        // without adding formatting overhead to the workload.
        for index in 0..iterations {
            let value = if index & 1 == 0 {
                "stream_market_data_update_payload_a"
            } else {
                "stream_market_data_update_payload_b"
            };
            self.sink
                .publish(value.to_owned())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_string_allocated_publish(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_string_conversion(&self, iterations: usize) {
        for _ in 0..iterations {
            let stored = crate::subscription::RtdValue::String(
                "stream_market_data_update_payload".to_owned(),
            )
            .into_stored()
            .expect("string conversion must succeed");
            std::hint::black_box(stored);
        }
    }

    #[inline]
    pub fn run_stored_publish(&self, iterations: usize) {
        let first =
            crate::subscription::RtdValue::String("stream_market_data_update_payload_a".to_owned())
                .into_stored()
                .expect("stored string conversion must succeed");
        let second =
            crate::subscription::RtdValue::String("stream_market_data_update_payload_b".to_owned())
                .into_stored()
                .expect("stored string conversion must succeed");
        for index in 0..iterations {
            let stored = if index & 1 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            self.sink
                .sink
                .publish_stored(stored)
                .expect("stored publish must succeed");
        }
    }

    #[inline]
    pub fn run_drain_each(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
            let batch = self
                .server
                .begin_refresh()
                .expect("begin_refresh must succeed");
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .expect("complete must succeed");
        }
    }
}

/// Candidate owned string representations for conversion-cost comparison.
///
/// `TriompheArcString` is the current production representation; the other
/// variants remain available so the ownership/conversion trade-off stays
/// directly measurable.
#[derive(Clone, Copy, Debug)]
pub enum RtdStringRepresentation {
    StdArcStr,
    StdArcString,
    TriompheArcString,
}

pub const RTD_STRING_REPRESENTATION_LENGTHS: [usize; 5] = [16, 35, 64, 256, 1024];

pub struct RtdStringRepresentationBenchmark {
    payload: String,
}

impl RtdStringRepresentationBenchmark {
    pub fn new(length: usize) -> Self {
        assert!(
            RTD_STRING_REPRESENTATION_LENGTHS.contains(&length),
            "benchmark must use a declared string length"
        );
        Self {
            payload: "x".repeat(length),
        }
    }

    #[hotpath::measure(impl_type = "RtdStringRepresentationBenchmark")]
    pub fn convert_std_arc_str(value: String) -> std::sync::Arc<str> {
        std::sync::Arc::from(value)
    }

    #[hotpath::measure(impl_type = "RtdStringRepresentationBenchmark")]
    pub fn convert_std_arc_string(value: String) -> std::sync::Arc<String> {
        std::sync::Arc::new(value)
    }

    #[hotpath::measure(impl_type = "RtdStringRepresentationBenchmark")]
    pub fn convert_triomphe_arc_string(value: String) -> triomphe::Arc<String> {
        triomphe::Arc::new(value)
    }

    #[inline]
    pub fn run(&self, representation: RtdStringRepresentation, iterations: usize) {
        for _ in 0..iterations {
            let value = self.payload.clone();
            match representation {
                RtdStringRepresentation::StdArcStr => {
                    std::hint::black_box(Self::convert_std_arc_str(value));
                }
                RtdStringRepresentation::StdArcString => {
                    std::hint::black_box(Self::convert_std_arc_string(value));
                }
                RtdStringRepresentation::TriompheArcString => {
                    std::hint::black_box(Self::convert_triomphe_arc_string(value));
                }
            }
        }
    }

    pub fn payload(&self) -> &str {
        &self.payload
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RtdRefreshValueKind {
    Number,
    ShortString,
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

pub const RTD_PARALLEL_CROSSING_CASES: [RtdRefreshScalingCase; 25] = [
    crossing_case("u0001_s01", 1, 1),
    crossing_case("u0032_s01", 32, 1),
    crossing_case("u0032_s02", 32, 2),
    crossing_case("u0032_s04", 32, 4),
    crossing_case("u0032_s08", 32, 8),
    crossing_case("u0032_s16", 32, 16),
    crossing_case("u0032_s32", 32, 32),
    crossing_case("u0128_s01", 128, 1),
    crossing_case("u0128_s02", 128, 2),
    crossing_case("u0128_s04", 128, 4),
    crossing_case("u0128_s08", 128, 8),
    crossing_case("u0128_s16", 128, 16),
    crossing_case("u0128_s32", 128, 32),
    crossing_case("u1024_s01", 1_024, 1),
    crossing_case("u1024_s02", 1_024, 2),
    crossing_case("u1024_s04", 1_024, 4),
    crossing_case("u1024_s08", 1_024, 8),
    crossing_case("u1024_s16", 1_024, 16),
    crossing_case("u1024_s32", 1_024, 32),
    crossing_case("u4096_s01", 4_096, 1),
    crossing_case("u4096_s02", 4_096, 2),
    crossing_case("u4096_s04", 4_096, 4),
    crossing_case("u4096_s08", 4_096, 8),
    crossing_case("u4096_s16", 4_096, 16),
    crossing_case("u4096_s32", 4_096, 32),
];

const fn crossing_case(
    name: &'static str,
    updated_topics: usize,
    ready_shards: usize,
) -> RtdRefreshScalingCase {
    RtdRefreshScalingCase {
        name,
        active_topics: 4_096,
        updated_topics,
        ready_shards,
    }
}

enum RtdRefreshSinks {
    Number(Vec<crate::subscription::RtdSink<f64>>),
    ShortString(Vec<crate::subscription::RtdSink<String>>),
}

pub struct RtdRefreshScalingBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::SubscriptionServerHandle,
    sinks: RtdRefreshSinks,
    updated_indices: Vec<usize>,
    parallel_pool: rayon::ThreadPool,
    expected_ready_shards: usize,
    expected_updates: usize,
    publish_revision: Cell<u64>,
}

impl RtdRefreshScalingBenchmark {
    pub fn new(case: RtdRefreshScalingCase, value_kind: RtdRefreshValueKind) -> Self {
        let parallel_threads = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
            .min(8);
        Self::with_parallel_threads(case, value_kind, parallel_threads)
    }

    pub fn with_parallel_threads(
        case: RtdRefreshScalingCase,
        value_kind: RtdRefreshValueKind,
        parallel_threads: usize,
    ) -> Self {
        assert!(case.active_topics > 0);
        assert!(case.updated_topics > 0);
        assert!(case.updated_topics <= case.active_topics);
        assert!((1..=TOPIC_SHARDS).contains(&case.ready_shards));
        assert!(parallel_threads > 0);

        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero benchmark server generation"),
            )
            .expect("server registration must succeed");
        let topic_ids = topic_ids_for_case(case);

        let sinks = match value_kind {
            RtdRefreshValueKind::Number => {
                RtdRefreshSinks::Number(connect_number_topics(&runtime, &server, &topic_ids))
            }
            RtdRefreshValueKind::ShortString => {
                RtdRefreshSinks::ShortString(connect_string_topics(&runtime, &server, &topic_ids))
            }
        };
        let updated_indices = (0..case.updated_topics).collect();
        let parallel_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(parallel_threads)
            .thread_name(|index| format!("xlfn-rtd-bench-{index}"))
            .build()
            .expect("benchmark Rayon pool must start");

        Self {
            _runtime: runtime,
            server,
            sinks,
            updated_indices,
            parallel_pool,
            expected_ready_shards: case.ready_shards,
            expected_updates: case.updated_topics,
            publish_revision: Cell::new(0),
        }
    }

    pub fn parallel_threads(&self) -> usize {
        self.parallel_pool.current_num_threads()
    }

    #[inline]
    pub fn publish_coalesced(&self) {
        self.publish_updates();
    }

    pub fn measure_refresh_planning(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let started = Instant::now();
            let batch = self
                .server
                .inner
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            measured += started.elapsed();
            drop(batch);
        }
        measured
    }

    pub fn measure_refresh_collection(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let planned = self
                .server
                .inner
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            let started = Instant::now();
            let batches = planned.publish.collect_refresh_batches(&planned.plan);
            measured += started.elapsed();
            drop(batches);
            drop(planned);
        }
        measured
    }

    pub fn measure_parallel_refresh_collection(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let planned = self
                .server
                .inner
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            let started = Instant::now();
            let batches = self.collect_parallel_batches(&planned);
            measured += started.elapsed();
            drop(batches);
            drop(planned);
        }
        measured
    }

    pub fn measure_refresh_reduction(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let planned = self
                .server
                .inner
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            let batches = planned.publish.collect_refresh_batches(&planned.plan);
            let started = Instant::now();
            let reduced = crate::subscription::reduce_refresh_batches(batches);
            measured += started.elapsed();
            drop(reduced);
            drop(planned);
        }
        measured
    }

    pub fn measure_parallel_refresh_reduction(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let planned = self
                .server
                .inner
                .publish
                .plan_refresh()
                .expect("refresh planning must succeed");
            let batches = self.collect_parallel_batches(&planned);
            let started = Instant::now();
            let reduced = crate::subscription::reduce_refresh_batches(batches);
            measured += started.elapsed();
            drop(reduced);
            drop(planned);
        }
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

    #[inline]
    pub fn run_parallel_end_to_end_cycle(&self) {
        self.publish_updates();
        let planned = self
            .server
            .inner
            .publish
            .plan_refresh()
            .expect("refresh planning must succeed");
        let batches = self.collect_parallel_batches(&planned);
        let reduced = crate::subscription::reduce_refresh_batches(batches);
        let batch = planned.finish_collection(reduced);
        batch
            .complete(crate::subscription::RefreshOutcome::Delivered)
            .expect("parallel refresh completion must succeed");
    }

    pub fn assert_parallel_collection_equivalent(&self) {
        self.publish_updates();
        let planned = self
            .server
            .inner
            .publish
            .plan_refresh()
            .expect("refresh planning must succeed");
        assert_eq!(
            planned.plan.candidate_shards.count_ones() as usize,
            self.expected_ready_shards,
            "benchmark shape must activate exactly the requested shards",
        );
        let sequential = crate::subscription::reduce_refresh_batches(
            planned.publish.collect_refresh_batches(&planned.plan),
        );
        let parallel =
            crate::subscription::reduce_refresh_batches(self.collect_parallel_batches(&planned));
        assert_eq!(
            sequential.len(),
            self.expected_updates,
            "benchmark shape must collect exactly the requested updates",
        );
        assert_eq!(sequential.len(), parallel.len());
        for (sequential, parallel) in sequential.iter().zip(&parallel) {
            assert_eq!(sequential.sequence, parallel.sequence);
            assert_eq!(sequential.topic_id, parallel.topic_id);
            assert_eq!(
                sequential.connection_generation,
                parallel.connection_generation
            );
            assert_eq!(sequential.value, parallel.value);
        }
        drop(parallel);
        planned
            .finish_collection(sequential)
            .complete(crate::subscription::RefreshOutcome::Delivered)
            .expect("equivalence snapshot completion must succeed");
    }

    fn collect_parallel_batches(
        &self,
        planned: &crate::subscription::PlannedRtdRefresh<'_, crate::excel_rtd::RtdSubscriptionHost>,
    ) -> Vec<crate::subscription::ShardRefreshBatch> {
        let shard_indices = candidate_shard_indices(planned.plan.candidate_shards);
        self.parallel_pool.install(|| {
            shard_indices
                .into_par_iter()
                .filter_map(|index| planned.publish.collect_shard(index))
                .collect()
        })
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
            RtdRefreshSinks::ShortString(sinks) => {
                let value = if revision & 1 == 0 {
                    "market-update-a"
                } else {
                    "market-update-b"
                };
                for &index in &self.updated_indices {
                    sinks[index]
                        .publish(value.to_owned())
                        .expect("string publish must succeed");
                }
            }
        }
    }
}

fn connect_number_topics(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    topic_ids: &[crate::subscription::TopicId],
) -> Vec<crate::subscription::RtdSink<f64>> {
    topic_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, topic_id)| connect_topic::<f64>(runtime, server, index, topic_id))
        .collect()
}

fn connect_string_topics(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    topic_ids: &[crate::subscription::TopicId],
) -> Vec<crate::subscription::RtdSink<String>> {
    topic_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, topic_id)| connect_topic::<String>(runtime, server, index, topic_id))
        .collect()
}

fn connect_topic<T>(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    index: usize,
    topic_id: crate::subscription::TopicId,
) -> crate::subscription::RtdSink<T>
where
    T: crate::subscription::IntoRtdValue + Clone + Send + Sync + 'static,
{
    let sink_slot = Arc::new(parking_lot::Mutex::new(None));
    let source = crate::subscription::RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("benchmark generation is non-zero"),
        BenchmarkRtdSource {
            sink: Arc::clone(&sink_slot),
        },
    )
    .expect("benchmark source handle allocation must succeed");
    let topic = crate::subscription::RtdTopic::single(format!("refresh-{index}"))
        .expect("benchmark RTD topic must be valid");
    let prepared = runtime
        .prepare(&source, topic)
        .expect("prepare must succeed");
    let id = prepared.id();
    let connection = runtime
        .connect_transaction(server, topic_id, id)
        .expect("connect_transaction must succeed");
    connection.commit().expect("connection commit must succeed");
    prepared.commit();
    sink_slot.lock().clone().expect("sink must be captured")
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

fn candidate_shard_indices(mut candidate_shards: u32) -> Vec<usize> {
    let mut indices = Vec::with_capacity(candidate_shards.count_ones() as usize);
    while candidate_shards != 0 {
        let index = candidate_shards.trailing_zeros() as usize;
        candidate_shards &= candidate_shards - 1;
        indices.push(index);
    }
    indices
}
