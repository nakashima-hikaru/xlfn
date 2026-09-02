#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use xlfn::prelude::*;
use xlfn::rtd::{RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue};

pub struct State {
    rtd: RtdSourceHandle<RtdFixtureSource>,
    core: Box<FixtureCore>,
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
        let core = Box::new(FixtureCore {
            sinks: Mutex::new(HashMap::new()),
            sequence: AtomicI32::new(0),
        });
        let core_ptr = NonNull::from(&*core);
        let rtd = context
            .rtd()
            .register_source(RtdFixtureSource { core: core_ptr })?;
        Ok(Opened::new(State { rtd, core }, (), ()))
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

struct FixtureCore {
    sinks: Mutex<HashMap<String, RtdSink<i32>>>,
    sequence: AtomicI32,
}

impl FixtureCore {
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

#[derive(Clone, Copy)]
struct RtdFixtureSource {
    core: NonNull<FixtureCore>,
}

// SAFETY: `FixtureCore` synchronizes internal mutation via `Mutex` and `AtomicI32`,
// so non-owning pointers may be safely sent across threads.
unsafe impl Send for RtdFixtureSource {}
// SAFETY: `FixtureCore` synchronizes all internal state.
unsafe impl Sync for RtdFixtureSource {}

impl RtdFixtureSource {
    fn core(&self) -> &FixtureCore {
        // SAFETY: `FixtureCore` is uniquely owned by `State` in `Addin::SharedState`.
        // The add-in lifecycle guarantees `State` outlives the RTD runtime, sources,
        // and all active subscriptions.
        unsafe { self.core.as_ref() }
    }
}

impl RtdSource for RtdFixtureSource {
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
        self.core()
            .sinks
            .lock()
            .map_err(|_| XllError::Panic)?
            .insert(key.clone(), sink);
        Ok(RtdFixtureSubscription {
            core: self.core,
            key,
        })
    }
}

struct RtdFixtureSubscription {
    core: NonNull<FixtureCore>,
    key: String,
}

// SAFETY: `FixtureCore` synchronizes internal mutation via `Mutex` and `AtomicI32`.
unsafe impl Send for RtdFixtureSubscription {}
// SAFETY: `FixtureCore` synchronizes all internal state.
unsafe impl Sync for RtdFixtureSubscription {}

impl RtdFixtureSubscription {
    fn core(&self) -> &FixtureCore {
        // SAFETY: `FixtureCore` is uniquely owned by `State` in `Addin::SharedState`,
        // which outlives the RTD runtime and this subscription.
        unsafe { self.core.as_ref() }
    }
}

// SAFETY: `disconnect_and_wait` removes the sink from the shared core before
// returning, ensuring no further sink usage after return.
unsafe impl RtdSubscription for RtdFixtureSubscription {
    fn request_cancel(&self) {
        // No background work to cancel in this fixture.
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.core()
            .sinks
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
    context.state().core.publish_batch(topic_count)
}

#[excel_function(name = "FRAMEWORK.RTD.ACTIVE", volatile)]
pub fn rtd_active(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, FixtureAddin>,
) -> XllResult<i32> {
    context.state().core.active_topics()
}

#[excel_function(name = "FRAMEWORK.VERSION", thread_safe)]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
