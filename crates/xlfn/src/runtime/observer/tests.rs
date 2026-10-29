use crate::runtime::Runtime;
use crate::runtime::shutdown::QuiescenceProof;

fn with_terminal_context(
    before_close: impl FnOnce(),
    operation: impl FnOnce(&Runtime<()>, &QuiescenceProof),
) {
    let _module = crate::ingress::acquire_test_module_lease();
    let _diagnostics = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
    crate::diagnostics::close_diagnostic_router().unwrap();
    let opening = crate::module_runtime::begin_open();
    crate::diagnostics::reset_diagnostic_router().unwrap();
    before_close();
    let drained = opening.rollback(|| {}).seal_and_drain().unwrap();
    let (module, _exports) = drained.certify_for_test();
    // Deliberately inject retained resources after the test certificate was
    // issued, to verify the observer does not replace live state with zeros.
    let proof = QuiescenceProof::for_test(None, module.id());
    operation(&Runtime::new(), &proof);
    crate::module_runtime::close_callbacks_for_test();
    crate::diagnostics::close_diagnostic_router().unwrap();
}

#[test]
#[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
fn terminal_snapshot_observes_host_and_return_obligations() {
    with_terminal_context(
        || {},
        |runtime, proof| {
            let deps = runtime.shutdown_deps();
            let script = crate::registration::host::test_support::CallbackScript::install([
                crate::registration::host::test_support::Reply::success(
                    xlfn_sys::XL_EVENT_REGISTER,
                    xlfn_sys::XLOPER12::integer(7),
                ),
            ]);
            let callbacks = crate::host_callback::HostCallbackSession::new();
            runtime
                .ensure_calculation_events(
                    &crate::registration::RegistrationHost::new(&callbacks),
                    false,
                )
                .unwrap();
            script.assert_calls(&[xlfn_sys::XL_EVENT_REGISTER]);
            let open = runtime.open_deps();
            open.returns().returns.reopen_admission().unwrap();
            let producer = open.returns().returns.try_enter_producer().unwrap();
            let control = runtime.lifecycle.access();
            let resources = deps.terminal_resources(&control, proof);
            assert_eq!(resources.resident_event_registrations, 1);
            assert_eq!(resources.return_blocks, 1);
            drop(control);
            drop(producer);
            open.returns().returns.close_admission();
            assert!(resources.module_residency_held);
            assert!(!runtime.release_module_residency().unwrap());
        },
    );
}

#[test]
fn terminal_snapshot_observes_open_callback_gate_and_retained_callbacks() {
    with_terminal_context(
        || {},
        |runtime, proof| {
            crate::module_runtime::reset_callbacks_for_test();
            let callback = crate::callback_gate::enter_callback().unwrap();
            let control = runtime.lifecycle.access();
            let resources = runtime.shutdown_deps().terminal_resources(&control, proof);
            assert!(resources.callback_gate_open);
            assert_eq!(resources.callbacks, 1);
            crate::module_runtime::close_callbacks_for_test();
            let resources = runtime.shutdown_deps().terminal_resources(&control, proof);
            assert!(!resources.callback_gate_open);
            assert_eq!(resources.callbacks, 1);
            drop(callback);
            let resources = runtime.shutdown_deps().terminal_resources(&control, proof);
            assert!(!resources.callback_gate_open);
            assert_eq!(resources.callbacks, 0);
        },
    );
}

#[cfg(feature = "async")]
#[test]
fn terminal_snapshot_observes_an_unstopped_async_executor() {
    with_terminal_context(
        || {},
        |runtime, proof| {
            runtime
                .async_runtime()
                .start(
                    crate::NoAsyncExecutor,
                    crate::addin::AsyncTaskLimit::DEFAULT,
                )
                .unwrap();
            let control = runtime.lifecycle.access();
            assert!(
                runtime
                    .shutdown_deps()
                    .terminal_resources(&control, proof)
                    .async_executor_running
            );
            drop(control);
            runtime.async_runtime().close().unwrap();
            let control = runtime.lifecycle.access();
            assert!(
                !runtime
                    .shutdown_deps()
                    .terminal_resources(&control, proof)
                    .async_executor_running
            );
        },
    );
}

#[test]
fn terminal_snapshot_observes_live_diagnostics_and_pending_delivery() {
    use crate::diagnostics::{DiagnosticEvent, DiagnosticSink};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    struct BlockingSink {
        started: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    impl DiagnosticSink for BlockingSink {
        fn report(&self, event: &DiagnosticEvent<'_>) {
            if event.udf_id() != "terminal observer" {
                return;
            }
            self.started.send(()).unwrap();
            self.resume.lock().unwrap().recv().unwrap();
        }
    }
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    with_terminal_context(
        || {
            crate::diagnostics::set_diagnostic_sink(BlockingSink {
                started: started_tx,
                resume: Mutex::new(resume_rx),
            })
            .unwrap();
            crate::diagnostics::report_no_unwind("terminal observer", &crate::XllError::Panic);
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        },
        |runtime, proof| {
            let control = runtime.lifecycle.access();
            let resources = runtime.shutdown_deps().terminal_resources(&control, proof);
            // Release the worker before assertions so a failing snapshot cannot
            // leave the global diagnostic router blocked during test cleanup.
            resume_tx.send(()).unwrap();
            assert!(resources.diagnostics_running);
            assert!(resources.diagnostics_pending >= 1);
        },
    );
}
