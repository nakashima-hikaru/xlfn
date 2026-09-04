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
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .expect("server registration must succeed");
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
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero test server generation"),
            )
            .expect("server registration must succeed");
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

    pub fn measure_refresh_collection(&self, iterations: u64) -> Duration {
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
            let batches = planned.publish.collect_refresh_batches(&planned.plan);
            measured += started.elapsed();
            planned
                .publish
                .restore_refresh_batches(&planned.plan, batches);
            planned.finished = true;
            drop(planned);
        }
        measured
    }

    pub fn measure_refresh_reduction(&self, iterations: u64) -> Duration {
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
            let batches = planned.publish.collect_refresh_batches(&planned.plan);
            let started = Instant::now();
            let reduced = crate::subscription::reduce_refresh_batches(batches);
            measured += started.elapsed();
            planned
                .publish
                .restore_refresh_updates(&planned.plan, reduced);
            planned.finished = true;
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
    let server = runtime
        .register_server(
            crate::subscription::ServerGeneration::new(1)
                .expect("non-zero benchmark server generation"),
        )
        .expect("server registration must succeed");
    let sinks = registered
        .into_iter()
        .zip(topic_ids.iter().copied())
        .enumerate()
        .map(|(index, ((source, sink_slot), topic_id))| {
            let topic = crate::subscription::RtdTopic::single(format!("refresh-{index}"))
                .expect("benchmark RTD topic must be valid");
            let prepared = runtime
                .prepare(&source, topic)
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
