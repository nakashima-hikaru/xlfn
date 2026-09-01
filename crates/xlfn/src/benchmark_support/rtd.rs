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
        let generation = crate::generation::RuntimeGeneration::new(1)
            .expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = Arc::new(
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
            server,
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
        let generation = crate::generation::RuntimeGeneration::new(1)
            .expect("benchmark generation is non-zero");
        let registration = crate::subscription::SourceRegistration::new(generation);
        let sink_slot = Arc::new(parking_lot::Mutex::new(None));
        let source = registration
            .register(BenchmarkRtdSource {
                sink: Arc::clone(&sink_slot),
            })
            .expect("benchmark source handle allocation must succeed");
        let runtime = Arc::new(
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
            server,
            sink,
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

enum RtdRefreshSinks {
    Number(Vec<crate::subscription::RtdSink<f64>>),
    ShortString(Vec<crate::subscription::RtdSink<String>>),
}

pub struct RtdRefreshScalingBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
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
            RtdRefreshValueKind::ShortString => {
                let (runtime, server, sinks) = build_refresh_topics::<String>(&topic_ids);
                (runtime, server, RtdRefreshSinks::ShortString(sinks))
            }
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
                .server()
                .expect("benchmark server remains registered")
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
                .server()
                .expect("benchmark server remains registered")
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

    pub fn measure_refresh_reduction(&self, iterations: u64) -> Duration {
        self.publish_updates();
        let mut measured = Duration::ZERO;
        for _ in 0..iterations {
            let planned = self
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

fn build_refresh_topics<T>(
    topic_ids: &[crate::subscription::TopicId],
) -> (
    Arc<crate::subscription::SubscriptionRuntime>,
    crate::subscription::SubscriptionServerHandle,
    Vec<crate::subscription::RtdSink<T>>,
)
where
    T: crate::subscription::IntoRtdValue + Clone + Send + Sync + 'static,
{
    let generation = crate::generation::RuntimeGeneration::new(1)
        .expect("benchmark generation is non-zero");
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
    let runtime = Arc::new(
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(
            registration.finish(),
        ),
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
