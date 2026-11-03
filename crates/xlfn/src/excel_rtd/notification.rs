//! Coalesced asynchronous RTD notices. Acceptance of UpdateNotify is not a
//! RefreshData acknowledgement, so an outstanding notice has a retry deadline.

use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::panic::AssertUnwindSafe;
use std::thread::{Builder, JoinHandle};
use std::time::{Duration, Instant};
use triomphe::Arc;

const RETRY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    stopping: bool,
    sequence: u64,
    pending: Option<u64>,
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
    pub(super) fn start(notify: impl FnMut() -> XllResult<()> + Send + 'static) -> XllResult<Self> {
        Self::start_with_interval(notify, RETRY_INTERVAL)
    }

    fn start_with_interval(
        mut notify: impl FnMut() -> XllResult<()> + Send + 'static,
        interval: Duration,
    ) -> XllResult<Self> {
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = Builder::new()
            .name("xlfn-rtd-notify".into())
            .spawn(move || {
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
                    let result =
                        crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(&mut notify))
                            .unwrap_or(Err(XllError::Panic));
                    if let Err(error) = &result {
                        if !matches!(error, XllError::Closing) && !reported_failure {
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
                    if matches!(result, Err(XllError::Closing | XllError::Panic)) {
                        state.stopping = true;
                        state.pending = None;
                    } else if state.pending == sequence {
                        // Delay from completion, not call entry: a slow COM
                        // call must not trigger an immediate burst of retries.
                        state.next_attempt = Some(Instant::now() + interval);
                    }
                }
            })
            .map_err(|error| XllError::Native {
                code: error.raw_os_error().unwrap_or(0),
                message: format!("failed to start RTD notification worker: {error}"),
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub(super) fn request(&self) -> XllResult<()> {
        let mut state = self.shared.state.lock();
        if state.stopping {
            return Err(XllError::Closing);
        }
        state.sequence = state.sequence.checked_add(1).ok_or(XllError::Overloaded)?;
        state.pending = Some(state.sequence);
        // Coalesce requests without postponing an already scheduled retry.
        self.shared.changed.notify_one();
        Ok(())
    }

    pub(super) fn pending(&self) -> Option<u64> {
        self.shared.state.lock().pending
    }

    pub(super) fn acknowledge(&self, sequence: Option<u64>) {
        let mut state = self.shared.state.lock();
        if state.pending == sequence {
            state.pending = None;
            state.next_attempt = None;
        }
    }

    pub(super) fn stop(&mut self) {
        {
            let mut state = self.shared.state.lock();
            state.stopping = true;
            state.pending = None;
            self.shared.changed.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            // The transport owner closes and drains its notification barrier
            // before dropping this pump; join also covers the worker's tail.
            if crate::panic_boundary::contain_panic(worker.join()).is_err() {
                xlfn_kernel::invariant::fail_stop();
            }
        }
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
                    XllError::Overloaded
                } else {
                    XllError::Closing
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
        assert!(matches!(pump.request(), Err(XllError::Closing)));
        assert!(received.try_recv().is_err());
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
            assert!(matches!(pump.request(), Err(XllError::Closing)));
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
