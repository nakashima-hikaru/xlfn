//! Coalesced asynchronous RTD notices. Acceptance of UpdateNotify is not a
//! RefreshData acknowledgement, so an outstanding notice has a retry deadline.

use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::num::NonZeroU64;
use std::panic::AssertUnwindSafe;
use std::thread::{Builder, JoinHandle};
use std::time::{Duration, Instant};
use triomphe::Arc;

const RETRY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    stopping: bool,
    sequence: u64,
    acknowledged: u64,
    pending: Option<NonZeroU64>,
    next_attempt: Option<Instant>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

pub(super) struct NotificationPump {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl NotificationPump {
    #[cfg(test)]
    pub(super) fn start(notify: impl FnMut() -> XllResult<()> + Send + 'static) -> XllResult<Self> {
        Self::start_with_interval(notify, RETRY_INTERVAL)
    }

    #[cfg(test)]
    fn start_with_interval(
        mut notify: impl FnMut() -> XllResult<()> + Send + 'static,
        interval: Duration,
    ) -> XllResult<Self> {
        Self::spawn(|| (), move |()| notify(), interval)
    }

    /// Construct and destroy the dispatch state on the worker, which also
    /// allows thread-affine resources that cannot implement Send.
    pub(super) fn start_with_state<S>(
        init: impl FnOnce() -> S + Send + 'static,
        notify: impl FnMut(&mut S) -> XllResult<()> + Send + 'static,
    ) -> XllResult<Self> {
        Self::spawn(init, notify, RETRY_INTERVAL)
    }

    fn spawn<S>(
        init: impl FnOnce() -> S + Send + 'static,
        mut notify: impl FnMut(&mut S) -> XllResult<()> + Send + 'static,
        interval: Duration,
    ) -> XllResult<Self> {
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let builder = Builder::new().name("xlfn-rtd-notify".into());
        // Even an idle worker can keep executing module code. Require a
        // definite spawn result before letting its owners unwind.
        let require_spawn_result =
            std::mem::DropGuard::new((), |()| xlfn_kernel::invariant::fail_stop());
        let spawn_result = builder.spawn(move || {
            // The state lock is declared later and released first at exit, so
            // worker resources also drop without the queue lock.
            let mut dispatch = init();
            let mut reported_failure = false;
            let mut state = worker_shared.state.lock();
            loop {
                if state.stopping {
                    return;
                }
                if state.pending.is_none() {
                    worker_shared.changed.wait(&mut state);
                    continue;
                }
                if let Some(deadline) = state.next_attempt {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if !remaining.is_zero() {
                        worker_shared.changed.wait_for(&mut state, remaining);
                        continue;
                    }
                }
                let sequence = state.pending;
                drop(state);
                // The host call can block or synchronously cause RefreshData.
                // Never hold the queue lock while running foreign code.
                let result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                    notify(&mut dispatch)
                }))
                .unwrap_or(Err(XllError::PANIC));
                if let Err(error) = &result {
                    if !matches!(error, __err if __err.is_closing()) && !reported_failure {
                        crate::diagnostics::report_no_unwind(
                            "RTD asynchronous UpdateNotify",
                            error,
                        );
                        reported_failure = true;
                    }
                } else {
                    reported_failure = false;
                }
                state = worker_shared.state.lock();
                if matches!(result, Err(error) if error.is_closing() || error.is_panic()) {
                    state.stopping = true;
                    state.pending = None;
                } else if state.pending == sequence {
                    // Delay from completion, not call entry: a slow COM
                    // call must not trigger an immediate burst of retries.
                    state.next_attempt = Some(Instant::now() + interval);
                }
            }
        });
        std::mem::DropGuard::dismiss(require_spawn_result);
        let worker = spawn_result.map_err(|error| {
            XllError::native(
                error.raw_os_error().unwrap_or(0),
                format!("failed to start RTD notification worker: {error}"),
            )
        })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub(super) fn request(&self) -> XllResult<()> {
        let mut state = self.shared.state.lock();
        if state.stopping {
            return Err(XllError::CLOSING);
        }
        let next = state
            .sequence
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(XllError::OVERLOADED)?;
        state.sequence = next.get();
        state.pending = Some(next);
        // Coalesce requests without postponing an already scheduled retry.
        self.shared.changed.notify_one();
        Ok(())
    }

    pub(super) fn pending(&self) -> Option<NonZeroU64> {
        self.shared.state.lock().pending
    }

    pub(super) fn acknowledge(&self, sequence: Option<NonZeroU64>) {
        let mut state = self.shared.state.lock();
        if let (Some(sequence), Some(pending)) = (sequence, state.pending)
            && state.acknowledged < sequence.get()
            && sequence <= pending
        {
            state.acknowledged = sequence.get();
            if sequence == pending {
                state.pending = None;
            }
            // A newer notice arrived during RefreshData; its first attempt
            // should not wait for the acknowledged notice's retry deadline.
            state.next_attempt = None;
            self.shared.changed.notify_one();
        }
    }

    pub(super) fn stop(&mut self) {
        // The transport owner drains admitted callbacks before dropping the
        // pump. Preserve its cookie if stopping or joining the worker unwinds.
        let require_join = std::mem::DropGuard::new((), |()| xlfn_kernel::invariant::fail_stop());
        {
            let mut state = self.shared.state.lock();
            state.stopping = true;
            state.pending = None;
            self.shared.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsHandle;
                if worker.thread().id() == std::thread::current().id() {
                    xlfn_kernel::invariant::fail_stop();
                }
                // Releasing the worker's GIT reference and leaving COM happen
                // on its thread, after the last admitted UpdateNotify. An STA
                // caller must keep dispatching until that thread has exited.
                // No queue or callback lock is held while dispatching.
                xlfn_host::com_wait::wait_with_com_pumping(
                    worker.as_handle(),
                    crate::win32::INFINITE,
                )
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            }
            if crate::panic_boundary::contain_panic(worker.join()).is_err() {
                xlfn_kernel::invariant::fail_stop();
            }
        }
        std::mem::DropGuard::dismiss(require_join);
    }
}

impl Drop for NotificationPump {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const DEADLINE: Duration = Duration::from_secs(2);

    #[test]
    fn acknowledging_older_refresh_clears_its_retry_deadline_only_once() {
        let pump = NotificationPump {
            shared: Arc::new(Shared::default()),
            worker: None,
        };
        pump.request().unwrap();
        let old = pump.pending();
        pump.request().unwrap();
        let new = pump.pending();
        let deadline = Instant::now() + DEADLINE;
        pump.shared.state.lock().next_attempt = Some(deadline);

        pump.acknowledge(old);
        assert_eq!(pump.pending(), new);
        assert_eq!(pump.shared.state.lock().next_attempt, None);
        pump.shared.state.lock().next_attempt = Some(deadline);
        pump.acknowledge(old);
        assert_eq!(pump.shared.state.lock().next_attempt, Some(deadline));
    }

    #[test]
    fn accepted_notice_retries_without_heartbeat_until_refresh_acknowledges() {
        let (sent, received) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let pump = NotificationPump::start_with_interval(
            move || {
                sent.send(()).unwrap();
                wait.recv_timeout(DEADLINE).unwrap();
                Ok(())
            },
            Duration::from_millis(20),
        )
        .unwrap();
        pump.request().unwrap();
        received.recv_timeout(DEADLINE).unwrap();
        release.send(()).unwrap();
        received.recv_timeout(DEADLINE).unwrap();
        pump.acknowledge(pump.pending());
        release.send(()).unwrap();
        assert_eq!(pump.pending(), None);
        assert_eq!(
            received.recv_timeout(Duration::from_millis(80)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn blocked_host_call_does_not_block_publish_or_erase_a_new_notice() {
        let (started, observed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let pump = NotificationPump::start(move || {
            started.send(()).unwrap();
            wait.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
        pump.request().unwrap();
        observed.recv_timeout(DEADLINE).unwrap();
        let old = pump.pending();
        // The producer can enqueue the rest of its topic batch even while
        // the first UpdateNotify is blocked in Excel's COM apartment.
        for _ in 0..100 {
            pump.request().unwrap();
        }
        let new = pump.pending();
        assert_ne!(new, old);
        pump.acknowledge(old);
        assert_eq!(pump.pending(), new);
        pump.acknowledge(new);
        release.send(()).unwrap();
    }

    #[test]
    fn refresh_acknowledgement_and_new_request_during_call_are_not_lost() {
        let (sent, received) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let pump = NotificationPump::start(move || {
            sent.send(()).unwrap();
            wait.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
        pump.request().unwrap();
        received.recv_timeout(DEADLINE).unwrap();
        pump.acknowledge(pump.pending());
        pump.request().unwrap();
        release.send(()).unwrap();
        received.recv_timeout(DEADLINE).unwrap();
        pump.acknowledge(pump.pending());
        release.send(()).unwrap();
    }

    #[test]
    fn errors_retry_at_bounded_rate_and_close_stops_retrying() {
        let (sent, received) = mpsc::channel();
        let mut calls = 0;
        let mut pump = NotificationPump::start_with_interval(
            move || {
                calls += 1;
                sent.send(Instant::now()).unwrap();
                Err(if calls == 1 {
                    XllError::OVERLOADED
                } else {
                    XllError::CLOSING
                })
            },
            Duration::from_millis(20),
        )
        .unwrap();
        pump.request().unwrap();
        let first = received.recv_timeout(DEADLINE).unwrap();
        let second = received.recv_timeout(DEADLINE).unwrap();
        assert!(second.duration_since(first) >= Duration::from_millis(20));
        pump.stop();
        assert!(matches!(pump.request(), Err(__err) if __err.is_closing()));
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn worker_state_is_affine_and_reused_after_a_failed_attempt() {
        use std::rc::Rc;
        use std::thread;

        struct WorkerState {
            calls: Rc<usize>,
            owner: thread::ThreadId,
            exited: mpsc::Sender<thread::ThreadId>,
        }

        impl Drop for WorkerState {
            fn drop(&mut self) {
                assert_eq!(self.owner, thread::current().id());
                self.exited.send(self.owner).unwrap();
            }
        }

        let (sent, received) = mpsc::channel();
        let (exited, joined) = mpsc::channel();
        let initializes = thread::current().id();
        let mut pump = NotificationPump::start_with_state(
            move || WorkerState {
                calls: Rc::new(0),
                owner: thread::current().id(),
                exited,
            },
            move |state| {
                let calls = Rc::get_mut(&mut state.calls).unwrap();
                *calls += 1;
                sent.send((*calls, thread::current().id())).unwrap();
                if *calls == 1 {
                    Err(XllError::OVERLOADED)
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();

        pump.request().unwrap();
        let (first, worker) = received.recv_timeout(DEADLINE).unwrap();
        assert_ne!(worker, initializes);
        assert_eq!(first, 1);
        assert_eq!(received.recv_timeout(DEADLINE).unwrap(), (2, worker));
        pump.acknowledge(pump.pending());
        pump.stop();
        assert_eq!(joined.try_recv().unwrap(), worker);
    }

    #[test]
    fn idle_pump_does_not_notify_and_stop_joins_the_callback_owner() {
        let (sent, received) = mpsc::channel::<()>();
        let mut pump = NotificationPump::start(move || {
            sent.send(()).unwrap();
            Ok(())
        })
        .unwrap();
        assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
        pump.stop();
        assert_eq!(received.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }

    #[test]
    fn stop_waits_for_an_in_flight_callback_and_prevents_further_calls() {
        let (sent, received) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let mut pump = NotificationPump::start(move || {
            sent.send(()).unwrap();
            wait.recv_timeout(DEADLINE).unwrap();
            Ok(())
        })
        .unwrap();
        pump.request().unwrap();
        received.recv_timeout(DEADLINE).unwrap();
        let (stopped, joined) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            pump.stop();
            assert!(matches!(pump.request(), Err(__err) if __err.is_closing()));
            stopped.send(()).unwrap();
        });
        assert_eq!(
            joined.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        release.send(()).unwrap();
        joined.recv_timeout(DEADLINE).unwrap();
        worker.join().unwrap();
        assert_eq!(received.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }
}
