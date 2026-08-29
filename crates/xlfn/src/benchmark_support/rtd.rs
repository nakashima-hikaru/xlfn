use super::*;
use std::time::Instant;

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
}

impl RtdRefreshScalingBenchmark {
    pub fn new(case: RtdRefreshScalingCase, value_kind: RtdRefreshValueKind) -> Self {
        assert!(case.active_topics > 0);
        assert!(case.updated_topics > 0);
        assert!(case.updated_topics <= case.active_topics);
        assert!((1..=TOPIC_SHARDS).contains(&case.ready_shards));

        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(
                crate::subscription::ServerGeneration::new(1)
                    .expect("non-zero benchmark server generation"),
            )
            .expect("server registration must succeed");

        let sinks = match value_kind {
            RtdRefreshValueKind::Number => RtdRefreshSinks::Number(connect_number_topics(
                &runtime,
                &server,
                case.active_topics,
            )),
            RtdRefreshValueKind::ShortString => RtdRefreshSinks::ShortString(
                connect_string_topics(&runtime, &server, case.active_topics),
            ),
        };
        let updated_indices = (0..case.updated_topics)
            .map(|ordinal| {
                let shard = ordinal % case.ready_shards;
                let row = ordinal / case.ready_shards;
                let index = row * TOPIC_SHARDS + shard;
                assert!(
                    index < case.active_topics,
                    "scaling case exceeds active topics"
                );
                index
            })
            .collect();

        Self {
            _runtime: runtime,
            server,
            sinks,
            updated_indices,
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
            let batch = planned.collect();
            measured += started.elapsed();
            drop(batch);
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
        match &self.sinks {
            RtdRefreshSinks::Number(sinks) => {
                for &index in &self.updated_indices {
                    sinks[index]
                        .publish(12.5)
                        .expect("number publish must succeed");
                }
            }
            RtdRefreshSinks::ShortString(sinks) => {
                for &index in &self.updated_indices {
                    sinks[index]
                        .publish("market-update".to_owned())
                        .expect("string publish must succeed");
                }
            }
        }
    }
}

fn connect_number_topics(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    active_topics: usize,
) -> Vec<crate::subscription::RtdSink<f64>> {
    (0..active_topics)
        .map(|index| connect_topic::<f64>(runtime, server, index))
        .collect()
}

fn connect_string_topics(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    active_topics: usize,
) -> Vec<crate::subscription::RtdSink<String>> {
    (0..active_topics)
        .map(|index| connect_topic::<String>(runtime, server, index))
        .collect()
}

fn connect_topic<T>(
    runtime: &Arc<crate::subscription::SubscriptionRuntime>,
    server: &crate::subscription::SubscriptionServerHandle,
    index: usize,
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
    let topic_id = i32::try_from(index + TOPIC_SHARDS).expect("benchmark topic id must fit i32");
    let connection = runtime
        .connect_transaction(server, crate::subscription::TopicId(topic_id), id)
        .expect("connect_transaction must succeed");
    connection.commit().expect("connection commit must succeed");
    prepared.commit();
    sink_slot.lock().clone().expect("sink must be captured")
}
