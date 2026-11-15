use super::{DrainWork, UnstartedDrain, WorkerJoined};
use crate::panic_boundary::{CaughtPanic, catch_no_unwind, contain_panic};
use std::mem::DropGuard;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{Builder, Scope, ScopedJoinHandle};
use xlfn_kernel::invariant::fail_stop;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

/// The helper exclusively owns the work and must be joined before teardown.
#[must_use = "join the helper before releasing the dispatch resources"]
pub(in crate::excel_rtd) struct JoinTransferred<'scope, T>(RollbackGuard<StartedHelper<'scope, T>>);

struct StartedHelper<'scope, T>(ScopedJoinHandle<'scope, Result<T, CaughtPanic>>);

impl<T> Rollback for StartedHelper<'_, T> {
    fn rollback(self) {
        // Scoped fallback joining could block the very STA needed by the
        // helper. Never let an unfinished dispatch manufacture completion.
        fail_stop();
    }
}

impl<T> JoinTransferred<'_, T> {
    pub(in crate::excel_rtd) fn join(self) -> WorkerJoined<T> {
        let StartedHelper(worker) = self.0.complete();
        let require_join = DropGuard::new((), |()| fail_stop());
        #[cfg(test)]
        FAIL_JOIN.with(|fault| assert!(!fault.replace(false), "injected drain join unwind"));
        let output = contain_panic(worker.join())
            .unwrap_or_else(|_| fail_stop())
            .unwrap_or_else(|_| fail_stop());
        DropGuard::dismiss(require_join);
        WorkerJoined(output)
    }
}

impl<W: DrainWork> UnstartedDrain<W> {
    pub(in crate::excel_rtd) fn spawn<'scope, 'env>(
        self,
        scope: &'scope Scope<'scope, 'env>,
        builder: Builder,
        signal_done: impl FnOnce() + Send + 'scope,
    ) -> Result<JoinTransferred<'scope, W::Output>, (std::io::Error, Self)>
    where
        W: 'scope,
        W::Output: 'scope,
    {
        // The single send fits before the helper reaches recv. An STA must
        // not rendezvous with startup before it can start pumping messages.
        let (send, receive) = mpsc::sync_channel::<W>(1);
        let started_or_exited = Arc::new(AtomicBool::new(false));
        let armed = Arc::clone(&started_or_exited);
        let completion = DropGuard::new(signal_done, move |signal_done| {
            if armed.swap(true, Ordering::AcqRel) {
                let require_signal = DropGuard::new((), |()| fail_stop());
                signal_done();
                DropGuard::dismiss(require_signal);
            }
        });
        // The closure owns both fields as one capture, including when a
        // startup hook unwinds before running it. Release the receiver and
        // any undelivered work before signaling completion in that case.
        let entry = (receive, completion);
        let run = move || run_helper(entry);

        // Unwinding from spawn says nothing about whether a helper started.
        // Abort before the scope could auto-join while the STA is blocked.
        let require_startup = DropGuard::new((), |()| fail_stop());
        #[cfg(test)]
        let fault = SPAWN_FAULT.with(|fault| fault.replace(None));
        #[cfg(test)]
        let started = if fault == Some(SpawnFault::Refused) {
            drop(run);
            Err(std::io::Error::other("injected drain worker spawn failure"))
        } else {
            builder.spawn_scoped(scope, run)
        };
        #[cfg(not(test))]
        let started = builder.spawn_scoped(scope, run);
        let worker = match started {
            Ok(worker) => worker,
            Err(error) => {
                DropGuard::dismiss(require_startup);
                return Err((error, self));
            }
        };
        let transferred = JoinTransferred(RollbackGuard::new(StartedHelper(worker)));
        #[cfg(test)]
        assert_ne!(
            fault,
            Some(SpawnFault::Unwind),
            "injected drain spawn unwind"
        );

        // Exactly one side sets this bit first. A pre-start exit which won
        // this race did not signal. A successfully started helper always does.
        if started_or_exited.swap(true, Ordering::AcqRel) {
            fail_stop();
        }
        send.send(self.0).unwrap_or_else(|_| fail_stop());
        DropGuard::dismiss(require_startup);
        Ok(transferred)
    }
}

fn run_helper<W: DrainWork, C>(
    (receive, _completion): (mpsc::Receiver<W>, C),
) -> Result<W::Output, CaughtPanic> {
    let work = receive.recv().unwrap_or_else(|_| fail_stop());
    drop(receive);
    catch_no_unwind(AssertUnwindSafe(|| work.run()))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpawnFault {
    Refused,
    Unwind,
}

#[cfg(test)]
std::thread_local! {
    static SPAWN_FAULT: std::cell::Cell<Option<SpawnFault>> = const { std::cell::Cell::new(None) };
    static FAIL_JOIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_drain_spawn() {
    SPAWN_FAULT.with(|fault| assert!(fault.replace(Some(SpawnFault::Refused)).is_none()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Probe<'a>(&'a AtomicUsize);

    impl Drop for Probe<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn miri_failed_drain_spawn_returns_the_unexecuted_owner_without_signaling() {
        let dropped = AtomicUsize::new(0);
        let signals = AtomicUsize::new(0);
        let probe = Probe(&dropped);
        let work = UnstartedDrain::new(move || drop(probe));
        let restored = std::thread::scope(|scope| {
            fail_next_drain_spawn();
            match work.spawn(scope, Builder::new(), || {
                signals.fetch_add(1, Ordering::Relaxed);
            }) {
                Err((_, restored)) => restored.into_work(),
                Ok(_) => panic!("definite spawn failure must return the work"),
            }
        });
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert_eq!(signals.load(Ordering::Relaxed), 0);
        drop(restored);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_completed_drain_signals_after_work_and_returns_its_owned_result() {
        let dropped = AtomicUsize::new(0);
        let signals = AtomicUsize::new(0);
        let probe = Probe(&dropped);
        let work = UnstartedDrain::new(move || {
            drop(probe);
            41
        });
        let joined = std::thread::scope(|scope| {
            work.spawn(scope, Builder::new(), || {
                assert_eq!(dropped.load(Ordering::Relaxed), 1);
                signals.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap_or_else(|_| panic!("helper starts"))
            .join()
        });
        assert_eq!(joined.into_output(), 41);
        assert_eq!(signals.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_unstarted_owner_retains_its_drop_obligation() {
        let dropped = AtomicUsize::new(0);
        let work = UnstartedDrain::new(Probe(&dropped));
        let probe = work.into_work();
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        drop(probe);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires a child process to observe fail-stop")]
    fn unknown_drain_startup_or_completion_cannot_return() {
        const CHILD: &str = "XLFN_DRAIN_WAIT_FAIL_STOP";
        if let Ok(path) = std::env::var(CHILD) {
            std::thread::scope(|scope| {
                if path == "spawn" {
                    SPAWN_FAULT.with(|fault| fault.set(Some(SpawnFault::Unwind)));
                }
                let work =
                    UnstartedDrain::new(|| assert_ne!(path, "work", "injected drain work unwind"));
                let worker = work
                    .spawn(scope, Builder::new(), || {})
                    .unwrap_or_else(|_| panic!("helper starts"));
                if path == "abandon" {
                    drop(worker);
                } else {
                    FAIL_JOIN.with(|fault| fault.set(path == "join"));
                    worker.join().into_output();
                }
            });
            return;
        }
        for path in ["spawn", "join", "abandon", "work"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "excel_rtd::drain_wait::threaded::tests::unknown_drain_startup_or_completion_cannot_return"])
                .env(CHILD, path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(
                    status.signal(),
                    Some(6),
                    "expected fail-stop SIGABRT on {path}"
                );
            }
            #[cfg(not(unix))]
            {
                assert!(!status.success());
                assert_ne!(
                    status.code(),
                    Some(101),
                    "must fail-stop rather than unwind on {path}"
                );
            }
        }
    }
}
