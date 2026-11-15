//! Transfers a drain owner to a scoped helper only after startup succeeds.
#[cfg(any(test, windows))]
mod threaded;
#[cfg(all(test, windows))]
pub(crate) use threaded::fail_next_drain_spawn;

pub(crate) trait DrainWork: Send {
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
pub(crate) struct UnstartedDrain<W>(W);

impl<W> UnstartedDrain<W> {
    pub(crate) fn new(work: W) -> Self {
        Self(work)
    }

    #[cfg(any(test, windows))]
    pub(crate) fn into_work(self) -> W {
        self.0
    }
}

#[cfg(not(windows))]
impl<W: DrainWork> UnstartedDrain<W> {
    pub(crate) fn run_inline(self) -> WorkerJoined<W::Output> {
        WorkerJoined(self.0.run())
    }
}

/// The work returned and, when dispatched, its helper finished joining.
#[must_use]
pub(crate) struct WorkerJoined<T>(T);

impl<T> WorkerJoined<T> {
    pub(crate) fn into_output(self) -> T {
        self.0
    }
}
