use super::{PoolShared, drop_runnable};
use async_task::Runnable;
use crossbeam_deque::Worker;
use crossbeam_utils::sync::Parker;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::Ordering;

thread_local! {
    static POLLER_POOL: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn is_current_poller(shared: &Arc<PoolShared>) -> bool {
    POLLER_POOL.get() == Arc::as_ptr(shared).addr()
}

pub(super) struct PollerExitGuard {
    pub(super) shared: Arc<PoolShared>,
    pub(super) local: Worker<Runnable>,
}

impl Drop for PollerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            while let Some(runnable) = self.local.pop() {
                self.shared.queue.injector.push(runnable);
            }
            self.shared.failed.store(true, Ordering::Release);
            self.shared.queue.wake_all();
        } else {
            debug_assert!(
                self.local.is_empty(),
                "normal poller exited with queued work"
            );
        }
        let last =
            xlfn_kernel::invariant::checked_atomic_dec_release(&self.shared.live_pollers) == 1;
        if last && self.shared.failed.load(Ordering::Acquire) {
            self.shared.queue.seal_and_wake_all();
            while let Some(runnable) = self.shared.queue.drain_abandoned() {
                drop_runnable(runnable);
            }
        }
        POLLER_POOL.set(0);
    }
}

fn find_task(index: usize, shared: &PoolShared, local: &Worker<Runnable>) -> Option<Runnable> {
    local
        .pop()
        .or_else(|| shared.queue.steal_injector_batch_and_pop(local))
        .or_else(|| shared.queue.steal_peer(index))
}

pub(super) fn run_poller(
    index: usize,
    shared: Arc<PoolShared>,
    local: Worker<Runnable>,
    parker: Parker,
) {
    POLLER_POOL.set(Arc::as_ptr(&shared).addr());
    let exit_guard = PollerExitGuard { shared, local };
    let bit = 1u64 << index;
    loop {
        if let Some(runnable) = find_task(index, &exit_guard.shared, &exit_guard.local) {
            #[cfg(test)]
            if let Some(hook) = exit_guard.shared.before_poll_hook.lock().clone() {
                hook();
            }
            runnable.run();
            continue;
        }
        exit_guard.shared.queue.announce_idle(bit);
        if let Some(runnable) = find_task(index, &exit_guard.shared, &exit_guard.local) {
            exit_guard
                .shared
                .queue
                .idle_pollers
                .fetch_and(!bit, Ordering::Relaxed);
            #[cfg(test)]
            if let Some(hook) = exit_guard.shared.before_poll_hook.lock().clone() {
                hook();
            }
            runnable.run();
            continue;
        }
        if exit_guard.shared.queue.is_sealed() {
            exit_guard
                .shared
                .queue
                .idle_pollers
                .fetch_and(!bit, Ordering::Relaxed);
            break;
        }
        parker.park();
        exit_guard
            .shared
            .queue
            .idle_pollers
            .fetch_and(!bit, Ordering::Relaxed);
    }
}
