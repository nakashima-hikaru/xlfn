use super::executor::ExecutorShared;
use super::task::TaskControl;
use crate::XllError;
use crate::cancellation::CancellationSource;
use async_channel::Receiver;
use async_task::Runnable;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) struct WorkerExitGuard {
    pub(crate) shared: Arc<ExecutorShared>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.shared
                .fatal_worker_failure
                .store(true, Ordering::Release);
        }
        let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.shared.live_workers);
        let _guard = self.shared.wait_lock.lock();
        self.shared.idle.notify_all();
    }
}

pub(crate) fn release_active(shared: &ExecutorShared) {
    if xlfn_kernel::invariant::checked_atomic_dec(&shared.active) == 1 {
        let _guard = shared.wait_lock.lock();
        shared.idle.notify_all();
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
