use super::*;

pub(crate) struct WorkerExitGuard {
    pub(crate) inner: Arc<ExecutorInner>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.inner
                .fatal_worker_failure
                .store(true, Ordering::Release);
        }
        self.inner.live_workers.fetch_sub(1, Ordering::AcqRel);
        let _guard = self.inner.wait_lock.lock();
        self.inner.idle.notify_all();
    }
}

pub(crate) fn release_active(inner: &ExecutorInner) {
    if inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
        let _guard = inner.wait_lock.lock();
        inner.idle.notify_all();
    }
}

pub(crate) fn cancelled_calculation_error() -> XllError {
    XllError::ExcelValue(crate::ExcelError::NotAvailable)
}

/// Cancels and aborts a batch of tasks outside of any lock.
pub(crate) fn cancel_tasks(tasks: Vec<TaskControl>) {
    for TaskControl {
        abort,
        cancellation,
    } in tasks
    {
        // CancellationToken is public and may have been polled by an arbitrary
        // executor. Its Waker::wake implementation is user-controlled and may
        // panic; one such panic must not prevent this or later tasks from being
        // aborted, especially while AsyncManager owns a Closing executor.
        cancel_source_no_unwind(&cancellation);
        let _ = catch_unwind(AssertUnwindSafe(|| abort.abort()));
    }
}

pub(crate) fn cancel_source_no_unwind(cancellation: &CancellationSource) {
    let _ = catch_unwind(AssertUnwindSafe(|| cancellation.cancel()));
}

pub(crate) fn run_executor(receiver: Receiver<Runnable>) {
    loop {
        let Ok(runnable) = receiver.recv_blocking() else {
            break;
        };
        // Panics from user futures are contained by the UDF wrapper. Anything
        // that reaches this executor boundary is an infrastructure failure and
        // must terminate the worker so close can report it and explicitly
        // dispose any work stranded after the last worker exits.
        runnable.run();
    }
}
