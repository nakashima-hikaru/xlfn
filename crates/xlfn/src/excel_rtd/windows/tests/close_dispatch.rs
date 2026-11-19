//! Native STA regressions for waits after logical completion was published.

use super::*;

fn run_in_child(name: &str) -> bool {
    const CHILD: &str = "XLFN_TEST_CLOSE_COM_DISPATCH";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .env(CHILD, name)
        .args([
            "--exact",
            &format!("excel_rtd::windows::tests::close_dispatch::{name}"),
        ])
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{name} failed: {status}");
            return false;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            child.wait().unwrap();
            panic!("{name} stopped dispatching COM during close");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

struct ExitProbe {
    request: mpsc::Sender<()>,
    completed: mpsc::Receiver<()>,
}

impl Drop for ExitProbe {
    fn drop(&mut self) {
        // A separate MTA makes the COM call, so even native TLS teardown only
        // sends/waits on channels and does not invoke COM from a destructor.
        self.request.send(()).unwrap();
        self.completed.recv().unwrap();
    }
}

thread_local! {
    static EXIT_PROBE: std::cell::RefCell<Option<ExitProbe>> = const {
        std::cell::RefCell::new(None)
    };
}

fn with_sta_call(on_call: impl FnOnce() + Send + 'static, operation: impl FnOnce(ExitProbe)) {
    let _apartment = TestComApartment::enter_sta();
    let sta = thread::current().id();
    let dispatched = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&dispatched);
    let probe = sta_probe::new(move || {
        assert_eq!(thread::current().id(), sta);
        observed.store(true, Ordering::Release);
        on_call();
    });
    // SAFETY: this thread owns an STA and keeps the interface registered until
    // the MTA caller and all native exit callbacks have finished.
    let git = unsafe { super::super::global_interface_table::get_git() }.unwrap();
    let mut cookie = 0;
    // SAFETY: probe/IID and writable cookie storage live through registration.
    assert_eq!(
        // SAFETY: the registered interface and output slot remain live here.
        unsafe { git.register(probe.as_ptr(), &IID_IUNKNOWN, &mut cookie) },
        S_OK
    );
    let (request, requests) = mpsc::channel();
    let (completed, completions) = mpsc::channel();
    let incoming = thread::spawn(move || {
        {
            let _apartment = TestComApartment::enter();
            let interface = sta_probe::interface(cookie);
            requests.recv().unwrap();
            sta_probe::call(&interface);
        }
        completed.send(()).unwrap();
    });
    operation(ExitProbe {
        request,
        completed: completions,
    });
    incoming.join().unwrap();
    assert!(dispatched.load(Ordering::Acquire));
    // SAFETY: no proxy user remains after the MTA thread has joined.
    assert_eq!(unsafe { git.revoke(cookie) }, S_OK);
}

#[test]
fn sta_coordinator_join_dispatches_native_exit_and_rejects_reentry() {
    if !run_in_child("sta_coordinator_join_dispatches_native_exit_and_rejects_reentry") {
        return;
    }
    let worker = Arc::new(TerminationWorker::default());
    let reentered = Arc::new(AtomicBool::new(false));
    let callback_worker = Arc::clone(&worker);
    let observed = Arc::clone(&reentered);
    with_sta_call(
        move || {
            observed.store(
                matches!(callback_worker.join(), Err(ServerCloseError::Reentrant)),
                Ordering::Release,
            );
        },
        |probe| {
            let start = worker.reserve_start().unwrap();
            start.commit(thread::spawn(move || {
                EXIT_PROBE.with(|slot| *slot.borrow_mut() = Some(probe));
            }));
            worker.seal_and_join().unwrap();
            assert_eq!(worker.phase(), TerminationWorkerStatus::Joined);
        },
    );
    assert!(reentered.load(Ordering::Acquire));
}

#[test]
fn concurrent_sta_coordinator_join_dispatches_while_another_joiner_owns_handle() {
    if !run_in_child("concurrent_sta_coordinator_join_dispatches_while_another_joiner_owns_handle")
    {
        return;
    }
    with_sta_call(
        || {},
        |probe| {
            let worker = Arc::new(TerminationWorker::default());
            let start = worker.reserve_start().unwrap();
            start.commit(thread::spawn(move || {
                EXIT_PROBE.with(|slot| *slot.borrow_mut() = Some(probe));
            }));
            let first_worker = Arc::clone(&worker);
            let first = thread::spawn(move || first_worker.seal_and_join());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while worker.phase() != TerminationWorkerStatus::Joining {
                assert!(
                    std::time::Instant::now() < deadline,
                    "first joiner did not claim handle"
                );
                thread::yield_now();
            }
            worker.seal_and_join().unwrap();
            first.join().unwrap().unwrap();
            assert_eq!(worker.phase(), TerminationWorkerStatus::Joined);
        },
    );
}

#[test]
fn coordinator_dispatch_preparation_failure_retains_worker_for_retry() {
    use crate::drain_wait::windows::{DrainPreparationStage, fail_next_drain_preparation};
    for failure in 0..3 {
        let worker = TerminationWorker::default();
        let start = worker.reserve_start().unwrap();
        let (release, released) = mpsc::channel();
        start.commit(thread::spawn(move || released.recv().unwrap()));
        match failure {
            0 => fail_next_drain_preparation(DrainPreparationStage::Apartment),
            1 => fail_next_drain_preparation(DrainPreparationStage::Event),
            _ => crate::drain_wait::fail_next_drain_spawn(),
        }
        assert!(matches!(
            worker.seal_and_join(),
            Err(ServerCloseError::WaitFailed(_))
        ));
        assert_eq!(worker.phase(), TerminationWorkerStatus::Running);
        assert!(worker.is_sealed());
        release.send(()).unwrap();
        worker.seal_and_join().unwrap();
        assert_eq!(worker.phase(), TerminationWorkerStatus::Joined);
    }
}

#[cfg(feature = "async-builtin")]
fn async_close_probe(native_exit: bool) {
    use crate::async_udf::{AsyncRuntime, BuiltinAsyncExecutor};
    use crate::cancellation::{CancellationGuarantee, CancellationSource};

    with_sta_call(
        || {},
        |probe| {
            let runtime = AsyncRuntime::new();
            let config = crate::BuiltinExecutorConfig::new()
                .with_poller_count(crate::AsyncPollerCount::new(1).unwrap());
            runtime
                .start(
                    BuiltinAsyncExecutor::new(config),
                    crate::AsyncTaskLimit::DEFAULT,
                )
                .unwrap();
            let (started, running) = mpsc::channel();
            runtime
                .submit(
                    runtime.current_epoch(),
                    async move {
                        if native_exit {
                            EXIT_PROBE.with(|slot| *slot.borrow_mut() = Some(probe));
                            started.send(()).unwrap();
                        } else {
                            let _probe = probe;
                            started.send(()).unwrap();
                            std::future::pending::<()>().await;
                        }
                    },
                    CancellationSource::new(CancellationGuarantee::BestEffort).0,
                )
                .unwrap();
            running.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(runtime.close().unwrap().issues.is_empty());
            assert!(runtime.is_stopped());
        },
    );
}

#[test]
#[cfg(feature = "async-builtin")]
fn sta_async_close_dispatches_during_task_destruction() {
    if run_in_child("sta_async_close_dispatches_during_task_destruction") {
        async_close_probe(false);
    }
}

#[test]
#[cfg(feature = "async-builtin")]
fn sta_async_close_dispatches_through_poller_native_exit() {
    if run_in_child("sta_async_close_dispatches_through_poller_native_exit") {
        async_close_probe(true);
    }
}
