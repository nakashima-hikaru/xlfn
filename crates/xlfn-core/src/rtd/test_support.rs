use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub(crate) enum TestNotifyOutcome {
    Success,
    Error(XllError),
    Panic,
}

pub(crate) struct TestNotifierState {
    pub(crate) calls: AtomicUsize,
    pub(crate) outcomes: Mutex<VecDeque<TestNotifyOutcome>>,
    pub(crate) entered: Mutex<Option<Sender<()>>>,
    pub(crate) release: Mutex<Option<Receiver<()>>>,
    pub(crate) panicking_drop: bool,
}

impl Default for TestNotifierState {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            outcomes: Mutex::new(VecDeque::new()),
            entered: Mutex::new(None),
            release: Mutex::new(None),
            panicking_drop: false,
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
        let outcome = self
            .outcomes
            .lock()
            .pop_front()
            .unwrap_or(TestNotifyOutcome::Success);
        match outcome {
            TestNotifyOutcome::Success => Ok(()),
            TestNotifyOutcome::Error(err) => Err(err),
            TestNotifyOutcome::Panic => panic!("injected test notifier panic"),
        }
    }
}
