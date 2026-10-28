use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub(crate) enum TestNotifyOutcome {
    Success,
    Error(XllError),
    Panic,
    PanicPayload(std::sync::Arc<AtomicUsize>),
}

pub(crate) struct TestNotifierState {
    pub(crate) calls: AtomicUsize,
    pub(crate) outcomes: Mutex<VecDeque<TestNotifyOutcome>>,
    pub(crate) entered: Mutex<Option<Sender<()>>>,
    pub(crate) release: Mutex<Option<Receiver<()>>>,
    pub(crate) panicking_drop: bool,
    pub(crate) callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Default for TestNotifierState {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            outcomes: Mutex::new(VecDeque::new()),
            entered: Mutex::new(None),
            release: Mutex::new(None),
            panicking_drop: false,
            callback: Mutex::new(None),
        }
    }
}

impl Drop for TestNotifierState {
    fn drop(&mut self) {
        if self.panicking_drop {
            panic!("test notifier drop panic");
        }
    }
}

impl TestNotifierState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn notify(&self) -> XllResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = self.entered.lock().as_ref() {
            let _ = entered.send(());
        }
        if let Some(release) = self.release.lock().as_ref() {
            let _ = release.recv();
        }
        let callback = self.callback.lock().take();
        if let Some(callback) = callback {
            callback();
        }
        let outcome = self
            .outcomes
            .lock()
            .pop_front()
            .unwrap_or(TestNotifyOutcome::Success);
        match outcome {
            TestNotifyOutcome::Success => Ok(()),
            TestNotifyOutcome::Error(err) => Err(err),
            TestNotifyOutcome::Panic => panic!("injected test notifier panic"),
            TestNotifyOutcome::PanicPayload(drops) => {
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(drops));
            }
        }
    }
}

/// Typed add-in fixture for tests that install concrete subscription owners.
pub(crate) struct TestRtdAddin<E>(std::marker::PhantomData<fn() -> E>);
impl<E: crate::RtdSubscription> crate::Addin for TestRtdAddin<E> {
    type RtdSubscription = E;
    type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = crate::NoAsyncExecutor;
    fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
        Ok(crate::Opened::new(()))
    }
}
