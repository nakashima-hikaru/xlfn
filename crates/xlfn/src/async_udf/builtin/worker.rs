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

// The final poller must acquire recovery publications from every previous
// exit. Keep both the checked decrement and its ordering identical in native
// execution and Loom; only the atomic backend changes.
macro_rules! release_poller {
    ($counter:expr) => {{
        let counter = &$counter;
        let mut previous = counter.load(Ordering::Acquire);
        loop {
            let next = previous
                .checked_sub(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            match counter.compare_exchange_weak(previous, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(previous) => break previous == 1,
                Err(current) => previous = current,
            }
        }
    }};
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
        // The final poller must acquire prior exits' recovery publications,
        // including failure and the runnables returned to the injector.
        let last = release_poller!(self.shared.live_pollers);
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

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
mod tests {
    use loom::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_final_poller_acquires_failed_peer_recovery_publication() {
        loom::model(|| {
            let count = Arc::new(AtomicUsize::new(2));
            let failed = Arc::new(AtomicBool::new(false));
            let abandoned = Arc::new(AtomicBool::new(false));
            let peer_count = Arc::clone(&count);
            let peer_failed = Arc::clone(&failed);
            let peer_abandoned = Arc::clone(&abandoned);
            let peer = loom::thread::spawn(move || {
                peer_abandoned.store(true, Ordering::Relaxed);
                peer_failed.store(true, Ordering::Release);
                if release_poller!(peer_count) {
                    assert!(peer_failed.load(Ordering::Acquire));
                    assert!(peer_abandoned.load(Ordering::Relaxed));
                }
            });
            if release_poller!(count) {
                assert!(failed.load(Ordering::Acquire));
                assert!(abandoned.load(Ordering::Relaxed));
            }
            peer.join().unwrap();
        });
    }
}
