//! Transfers a drain owner to a scoped helper only after startup succeeds.
#[cfg(any(test, windows))]
mod threaded;
#[cfg(any(test, all(windows, feature = "dispatch-test-hooks")))]
pub use threaded::fail_next_drain_spawn;

#[cfg(windows)]
pub mod windows;

pub trait DrainWork: Send {
    type Output: Send;

    fn run(self) -> Self::Output;
}

impl<F, T> DrainWork for F
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    type Output = T;

    fn run(self) -> Self::Output {
        self()
    }
}

/// Retains exclusive ownership until the dispatch helper can receive it.
#[must_use]
pub struct UnstartedDrain<W>(W);

impl<W> UnstartedDrain<W> {
    pub fn new(work: W) -> Self {
        Self(work)
    }

    pub fn into_work(self) -> W {
        self.0
    }
}

#[cfg(not(windows))]
impl<W: DrainWork> UnstartedDrain<W> {
    pub fn run_inline(self) -> WorkerJoined<W::Output> {
        WorkerJoined(self.0.run())
    }
}

/// The work returned and, when dispatched, its helper finished joining.
#[must_use]
pub struct WorkerJoined<T>(T);

impl<T> WorkerJoined<T> {
    pub fn into_output(self) -> T {
        self.0
    }
}
