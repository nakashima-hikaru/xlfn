use super::executor::ExecutorShared;
use super::task::TaskControl;
use crate::XllError;
use crate::cancellation::CancellationSource;
use async_task::Runnable;
use crossbeam_deque::Worker;
use crossbeam_utils::sync::Parker;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) struct WorkerExitGuard {
    pub(crate) shared: Arc<ExecutorShared>,
    pub(crate) local: Option<Worker<Runnable>>,
}

impl WorkerExitGuard {
    fn recover_local_queue(&mut self) {
        if let Some(local) = self.local.take() {
            let mut returned = 0;
            while let Some(runnable) = local.pop() {
                self.shared.queue.injector.push(runnable);
                returned += 1;
            }
            if returned > 0 {
                self.shared.queue.wake_one();
            }
        }
    }
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.recover_local_queue();
            self.shared
                .fatal_worker_failure
                .store(true, Ordering::Release);
        } else {
            // Normal workers exit only when the queue is sealed and empty.
            debug_assert!(
                self.local.as_ref().is_none_or(|l| l.is_empty()),
                "normal worker exited with non-empty local queue"
            );
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

fn find_task(
    worker_index: usize,
    shared: &ExecutorShared,
    local: &Worker<Runnable>,
) -> Option<Runnable> {
    if let Some(runnable) = local.pop() {
        return Some(runnable);
    }
    if let Some(runnable) = shared.queue.steal_injector_batch_and_pop(local) {
        return Some(runnable);
    }
    if let Some(runnable) = shared.queue.steal_peer(worker_index) {
        return Some(runnable);
    }
    None
}

pub(crate) fn run_executor(
    worker_index: usize,
    shared: Arc<ExecutorShared>,
    local: Worker<Runnable>,
    parker: Parker,
) {
    let exit_guard = WorkerExitGuard {
        shared: Arc::clone(&shared),
        local: Some(local),
    };
    let my_bit = 1u64 << worker_index;

    loop {
        let local_ref = exit_guard.local.as_ref().unwrap();
        if let Some(runnable) = find_task(worker_index, &shared, local_ref) {
            runnable.run();
            continue;
        }

        shared.queue.idle_workers.fetch_or(my_bit, Ordering::AcqRel);

        let local_ref = exit_guard.local.as_ref().unwrap();
        if let Some(runnable) = find_task(worker_index, &shared, local_ref) {
            shared
                .queue
                .idle_workers
                .fetch_and(!my_bit, Ordering::AcqRel);
            runnable.run();
            continue;
        }

        if shared.queue.is_sealed() {
            shared
                .queue
                .idle_workers
                .fetch_and(!my_bit, Ordering::AcqRel);
            break;
        }

        parker.park();
        shared
            .queue
            .idle_workers
            .fetch_and(!my_bit, Ordering::AcqRel);
    }
}
