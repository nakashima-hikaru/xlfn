#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use xlfn::prelude::*;
use xlfn::rtd::{
    RtdCancellation, RtdCancellationHandle, RtdSink, RtdSource, RtdSubscription, RtdTopic,
    RtdValue,
};

pub struct State {
    rtd: RtdSourceHandle<RtdFixture>,
    publisher: Arc<RtdFixture>,
}

#[excel_addin(
    name = "Excel XLL Framework E2E Fixture",
    id = "xlfn-e2e-fixture",
    category = "Framework E2E"
)]
pub struct FixtureAddin;

impl Addin for FixtureAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        context: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        let publisher = Arc::new(RtdFixture::default());
        Ok(Opened::new(State {
            rtd: context
                .rtd()
                .register_shared_source(Arc::clone(&publisher))?,
            publisher,
        }, (), ()))
    }
}

#[derive(ExcelHandleObject)]
pub struct FixtureObject {
    value: i32,
}

#[excel_function(name = "FRAMEWORK.HANDLE.CREATE")]
pub fn create_handle(value: i32) -> FixtureObject {
    FixtureObject { value }
}

#[excel_function(name = "FRAMEWORK.HANDLE.VALUE")]
pub fn handle_value(value: Handle<'_, FixtureObject>) -> i32 {
    value.value
}

#[excel_function(name = "FRAMEWORK.ASYNC.ADD")]
pub async fn async_add(x: f64, y: f64) -> XllResult<f64> {
    let result = x + y;
    if result.is_finite() {
        Ok(result)
    } else {
        Err(XllError::Domain {
            code: xlfn::error::DomainErrorCode::InvalidInput,
        })
    }
}

#[derive(Default)]
struct RtdFixture {
    sinks: Arc<Mutex<HashMap<String, RtdSink<i32>>>>,
    sequence: AtomicI32,
}

impl RtdFixture {
    fn publish_batch(&self, topic_count: i32) -> XllResult<i32> {
        if !(1..=3).contains(&topic_count) {
            return Err(XllError::input(
                "topic_count",
                xlfn::error::InputError::OutOfRange,
            ));
        }
        let sinks = self.sinks.lock().map_err(|_| XllError::Panic)?;
        let batch = (1..=topic_count)
            .map(|topic_id| sinks.get(&format!("topic-{topic_id}")).cloned())
            .collect::<Option<Vec<_>>>()
            .ok_or(XllError::ExcelValue(ExcelError::NotAvailable))?;
        drop(sinks);

        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        for (offset, sink) in batch.into_iter().enumerate() {
            sink.publish(sequence * 100 + offset as i32 + 1)?;
        }
        Ok(sequence)
    }

    fn active_topics(&self) -> XllResult<i32> {
        let count = self
            .sinks
            .lock()
            .map_err(|_| XllError::Panic)?
            .len();
        i32::try_from(count).map_err(|_| XllError::Domain {
            code: xlfn::error::DomainErrorCode::Overflow,
        })
    }
}

impl RtdSource for RtdFixture {
    type Value = i32;
    type Subscription = RtdFixtureSubscription;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription> {
        let key = topic
            .parts()
            .first()
            .cloned()
            .ok_or(XllError::InvalidHandle)?;
        self.sinks
            .lock()
            .map_err(|_| XllError::Panic)?
            .insert(key.clone(), sink);
        Ok(RtdFixtureSubscription {
            sinks: Arc::clone(&self.sinks),
            key,
        })
    }
}

struct RtdFixtureSubscription {
    sinks: Arc<Mutex<HashMap<String, RtdSink<i32>>>>,
    key: String,
}

impl RtdSubscription for RtdFixtureSubscription {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        Arc::new(RtdCancellationHandle::noop())
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.sinks
            .lock()
            .map_err(|_| XllError::Panic)?
            .remove(&self.key);
        Ok(())
    }
}

#[excel_function(name = "FRAMEWORK.RTD.FIXTURE")]
pub fn rtd_fixture(
    #[excel_context(main_thread)] context: MainThreadContext<'_, FixtureAddin>,
    topic_id: i32,
) -> XllResult<RtdValue> {
    if !(1..=3).contains(&topic_id) {
        return Err(XllError::input(
            "topic_id",
            xlfn::error::InputError::OutOfRange,
        ));
    }
    context.rtd().subscribe(
        &context.state().rtd,
        RtdTopic::single(format!("topic-{topic_id}"))?,
    )
}

#[excel_function(name = "FRAMEWORK.RTD.PUBLISH", volatile)]
pub fn rtd_publish(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, FixtureAddin>,
    topic_count: i32,
) -> XllResult<i32> {
    context.state().publisher.publish_batch(topic_count)
}

#[excel_function(name = "FRAMEWORK.RTD.ACTIVE", volatile)]
pub fn rtd_active(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, FixtureAddin>,
) -> XllResult<i32> {
    context.state().publisher.active_topics()
}

#[excel_function(name = "FRAMEWORK.VERSION", thread_safe)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
