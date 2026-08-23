use super::*;

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
        let key = *prepared.key();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(1), &key)
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
        let key = *prepared.key();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(2), &key)
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
