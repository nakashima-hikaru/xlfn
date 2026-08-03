#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use xlfn::prelude::*;

pub struct State {
    rtd: Arc<RtdFixture>,
}

#[excel_addin(
    name = "Excel XLL Framework E2E Fixture",
    id = "xlfn-e2e-fixture",
    category = "Framework E2E"
)]
pub struct FixtureAddin;

impl Addin for FixtureAddin {
    type State = State;
    type Error = XllError;

    fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State {
            rtd: Arc::new(RtdFixture::default()),
        })
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
pub fn handle_value(value: Handle<FixtureObject>) -> i32 {
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
        let sinks = self.sinks.lock().map_err(|_| XllError::Internal {
            diagnostic_id: 0x5254_4446_4958_4c4b,
        })?;
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
            .map_err(|_| XllError::Internal {
                diagnostic_id: 0x5254_4446_4958_4c4b,
            })?
            .len();
        i32::try_from(count).map_err(|_| XllError::Domain {
            code: xlfn::error::DomainErrorCode::Overflow,
        })
    }
}

impl RtdSource for RtdFixture {
    type Value = i32;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>> {
        let key = topic
            .parts()
            .first()
            .cloned()
            .ok_or(XllError::InvalidHandle)?;
        self.sinks
            .lock()
            .map_err(|_| XllError::Internal {
                diagnostic_id: 0x5254_4446_4958_4c4b,
            })?
            .insert(key.clone(), sink);
        Ok(Box::new(RtdFixtureSubscription {
            sinks: Arc::clone(&self.sinks),
            key,
        }))
    }
}

struct RtdFixtureSubscription {
    sinks: Arc<Mutex<HashMap<String, RtdSink<i32>>>>,
    key: String,
}

// SAFETY: this fixture starts no background work. Removing the sink
// synchronously makes the subscription quiescent before returning.
unsafe impl RtdSubscription for RtdFixtureSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.sinks
            .lock()
            .map_err(|_| XllError::Internal {
                diagnostic_id: 0x5254_4446_4958_4c4b,
            })?
            .remove(&self.key);
        Ok(())
    }
}

#[excel_function(name = "FRAMEWORK.RTD.FIXTURE")]
pub fn rtd_fixture(
    #[excel_context(main_thread)] context: MainThreadContext<'_, '_, State>,
    topic_id: i32,
) -> XllResult<RtdValue> {
    if !(1..=3).contains(&topic_id) {
        return Err(XllError::input(
            "topic_id",
            xlfn::error::InputError::OutOfRange,
        ));
    }
    context.subscribe(
        Arc::clone(&context.state().rtd),
        RtdTopic::single(format!("topic-{topic_id}"))?,
    )
}

#[excel_function(name = "FRAMEWORK.RTD.PUBLISH", thread_safe, volatile)]
pub fn rtd_publish(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, State>,
    topic_count: i32,
) -> XllResult<i32> {
    context.state().rtd.publish_batch(topic_count)
}

#[excel_function(name = "FRAMEWORK.RTD.ACTIVE", thread_safe, volatile)]
pub fn rtd_active(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, State>,
) -> XllResult<i32> {
    context.state().rtd.active_topics()
}

#[excel_function(name = "FRAMEWORK.VERSION", thread_safe)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
