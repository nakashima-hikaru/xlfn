//! Inject native calculation callbacks at the actual unregister boundary.

use super::host::{host_auto_close, host_auto_open, host_auto_remove};
use crate::__private::v1::{MacroRuntime, end_calculation, export_void_boundary};
use crate::diagnostics::AddinId;
use crate::lifecycle::LifecyclePhase;
use crate::registration::host::test_support::{CallbackScript, Reply};
use crate::registration::{RegistrationDescriptor, RegistrationSignature};
use crate::runtime::StaticTestRuntime;
use std::sync::{Arc, Mutex};
use xlfn_common::{ExecutionKind, FunctionVisibility};
use xlfn_sys::{
    XL_EVENT_REGISTER, XL_FREE, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME,
    XLF_UNREGISTER, XLOPER12, XLOPER12Value, XLRET_ABORT, XLRET_SUCCESS, XLRET_UNCALCED,
    XLTYPE_STR,
};

struct EventAddin;

impl crate::Addin for EventAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = crate::XllError;
    type Layers = ();
    #[cfg(feature = "async-builtin")]
    type AsyncExecutor = crate::BuiltinAsyncExecutor;
    #[cfg(all(feature = "async", not(feature = "async-builtin")))]
    type AsyncExecutor = crate::NoAsyncExecutor;

    fn open(_: &crate::OpenContext) -> crate::OpenResult<Self> {
        let opened = crate::Opened::new(());
        #[cfg(feature = "async-builtin")]
        let opened = opened.with_async_executor(crate::BuiltinAsyncExecutor::new(
            crate::BuiltinExecutorConfig::new()
                .with_poller_count(crate::AsyncPollerCount::new(32).unwrap()),
        ));
        Ok(opened)
    }
}

fn module_name() -> XLOPER12 {
    #[cfg(not(target_os = "windows"))]
    static NAME: [u16; 10] = [9, 47, 116, 101, 115, 116, 46, 120, 108, 108];
    #[cfg(target_os = "windows")]
    static NAME: [u16; 9] = [8, 67, 58, 92, 116, 46, 120, 108, 108];
    XLOPER12 {
        value: XLOPER12Value {
            string: NAME.as_ptr().cast_mut(),
        },
        xltype: XLTYPE_STR,
    }
}

fn opening_replies() -> Vec<Reply> {
    let mut replies = vec![Reply::success(XL_GET_NAME, module_name())];
    #[cfg(feature = "async")]
    replies.push(Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(1)));
    replies.extend([
        Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(1)),
        Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
        Reply::success(XLF_REGISTER, XLOPER12::number(7.0)),
    ]);
    replies
}

fn opening_calls() -> Vec<i32> {
    let mut calls = vec![XL_GET_NAME];
    #[cfg(feature = "async")]
    calls.push(XL_EVENT_REGISTER);
    calls.extend([XL_EVENT_REGISTER, XLF_EVALUATE, XLF_REGISTER]);
    calls
}

fn open(runtime: &'static MacroRuntime<EventAddin>) {
    runtime
        .runtime()
        .ensure_module_residency(
            host_auto_close::<EventAddin> as fn(&crate::runtime::Runtime<EventAddin>) -> i32
                as *const (),
        )
        .unwrap();
    assert_eq!(
        host_auto_open(
            runtime.runtime(),
            &AddinId::parse("event-shutdown").unwrap(),
            "0",
            "test",
            &[RegistrationDescriptor {
                export_name: "test_event_shutdown",
                excel_name: "TEST.EVENT.SHUTDOWN",
                signature: RegistrationSignature {
                    return_abi: xlfn_common::ReturnAbi::Value12,
                    execution: if cfg!(feature = "async") {
                        ExecutionKind::Async
                    } else {
                        ExecutionKind::MainThread
                    },
                    arguments: &[],
                    volatile: false,
                },
                category: "Test",
                description: "events during registration cleanup",
                help_topic: "",
                visibility: FunctionVisibility::Public,
                arguments: &[],
            }],
        ),
        1,
    );
    // Establish that this exact export reaches a live generation before
    // asserting that its later invocations are rejected.
    let before = runtime.runtime().calculation_id().get();
    export_void_boundary(|| end_calculation(runtime));
    assert_eq!(runtime.runtime().calculation_id().get(), before + 1);
}

#[derive(Debug)]
struct EventObservation {
    phase: LifecyclePhase,
    ingress_phase: u8,
    active_exports: usize,
    before: u64,
    after: u64,
    #[cfg(feature = "async")]
    executor_stopped: bool,
}

fn deliver_events(runtime: &'static MacroRuntime<EventAddin>) -> EventObservation {
    let before = runtime.runtime().calculation_id().get();
    // Match the generated exports' two boundaries, including panic containment.
    export_void_boundary(|| end_calculation(runtime));
    #[cfg(feature = "async")]
    export_void_boundary(|| crate::__private::v1::cancel_async_calculation(runtime));
    EventObservation {
        phase: runtime.runtime().phase(),
        ingress_phase: crate::module_runtime::ingress().phase(),
        active_exports: crate::module_runtime::ingress().active_calls(),
        before,
        after: runtime.runtime().calculation_id().get(),
        #[cfg(feature = "async")]
        executor_stopped: runtime.runtime().async_runtime().is_stopped(),
    }
}

fn inject_events(
    script: &CallbackScript,
    function: i32,
    runtime: &'static MacroRuntime<EventAddin>,
) -> Arc<Mutex<Vec<EventObservation>>> {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&observed);
    script.on_call(function, move || {
        let same_thread = deliver_events(runtime);
        let other_thread = std::thread::spawn(move || deliver_events(runtime))
            .join()
            .unwrap();
        output.lock().unwrap().extend([same_thread, other_thread]);
    });
    observed
}

fn assert_rejected(observation: &EventObservation, phase: LifecyclePhase) {
    assert_eq!(observation.phase, phase);
    assert_eq!(observation.ingress_phase, crate::ingress::PHASE_CLOSED);
    assert_eq!(observation.active_exports, 0);
    assert_eq!(observation.before, observation.after);
    #[cfg(feature = "async")]
    assert!(observation.executor_stopped);
}

#[test]
fn calculation_events_reentered_from_each_unregister_callback_are_inert() {
    for remove in [false, true] {
        for callback in [XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME, XL_FREE] {
            let mut replies = opening_replies();
            replies.extend([
                Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
                Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
            ]);
            let script = CallbackScript::install(replies);
            let runtime: &'static _ = Box::leak(Box::new(MacroRuntime::<EventAddin>::new()));
            let _fixture = StaticTestRuntime::from_static(runtime.runtime());
            open(runtime);
            let observed = inject_events(&script, callback, runtime);
            let status = if remove {
                host_auto_remove(runtime.runtime())
            } else {
                host_auto_close(runtime.runtime())
            };
            assert_eq!(status, 1);
            assert_eq!(runtime.runtime().phase(), LifecyclePhase::Closed);
            assert!(!runtime.runtime().has_current_generation());
            let observed = observed.lock().unwrap();
            assert_eq!(observed.len(), 2, "callback={callback}");
            for event in &*observed {
                assert_rejected(event, LifecyclePhase::Closing);
            }
            assert_rejected(&deliver_events(runtime), LifecyclePhase::Closed);
            let mut calls = opening_calls();
            calls.extend([XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME]);
            script.assert_calls(&calls);
        }
    }
}

#[test]
fn reentered_events_do_not_enable_further_callbacks_after_unregister_errors() {
    for callback in [XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME] {
        for status in [XLRET_ABORT, XLRET_UNCALCED] {
            let mut replies = opening_replies();
            let mut calls = opening_calls();
            for (function, value) in [
                (XLF_UNREGISTER, XLOPER12::boolean(true)),
                (XLF_EVALUATE, XLOPER12::number(7.0)),
                (XLF_SET_NAME, XLOPER12::boolean(true)),
            ] {
                calls.push(function);
                replies.push(
                    Reply::success(function, value).status(if function == callback {
                        status
                    } else {
                        XLRET_SUCCESS
                    }),
                );
                if function == callback {
                    break;
                }
            }
            let script = CallbackScript::install(replies);
            let runtime: &'static _ = Box::leak(Box::new(MacroRuntime::<EventAddin>::new()));
            let _fixture = StaticTestRuntime::from_static(runtime.runtime());
            // Quarantine retains the production router; release fixture-owned
            // process-global diagnostics after checking that path.
            let _diagnostics = scopeguard::guard((), |_| {
                let _ = crate::diagnostics::close_diagnostic_router();
            });
            open(runtime);
            let observed = inject_events(&script, callback, runtime);
            assert_eq!(host_auto_close(runtime.runtime()), 1);
            let phase = if callback == XLF_UNREGISTER {
                LifecyclePhase::Quarantined
            } else {
                LifecyclePhase::Closed
            };
            assert_eq!(runtime.runtime().phase(), phase);
            let observed = observed.lock().unwrap();
            assert_eq!(observed.len(), 2);
            for event in &*observed {
                assert_rejected(event, LifecyclePhase::Closing);
            }
            assert_rejected(&deliver_events(runtime), phase);
            // No new Excel session or callback may be created by the event.
            script.assert_calls(&calls);
        }
    }
}

#[cfg(feature = "async-builtin")]
#[test]
fn an_event_admitted_before_close_finishes_before_unregister_begins() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let mut replies = opening_replies();
    replies.extend([
        Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
        Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
    ]);
    let script = CallbackScript::install(replies);
    let runtime: &'static _ = Box::leak(Box::new(MacroRuntime::<EventAddin>::new()));
    let _fixture = StaticTestRuntime::from_static(runtime.runtime());
    open(runtime);
    let before = runtime.runtime().calculation_id().get();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    runtime
        .runtime()
        .async_runtime()
        .set_before_calculation_transition_hook(Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        })));
    let event = std::thread::spawn(move || {
        export_void_boundary(|| end_calculation(runtime));
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (unregister_tx, unregister_rx) = mpsc::channel();
    let unregister_state = Arc::new(Mutex::new(None));
    let captured_state = Arc::clone(&unregister_state);
    script.on_call(XLF_UNREGISTER, move || {
        *captured_state.lock().unwrap() = Some((
            crate::module_runtime::ingress().active_calls(),
            runtime.runtime().calculation_id().get(),
            runtime.runtime().async_runtime().is_stopped(),
        ));
        // The observer can finish immediately after releasing the event.
        let _ = unregister_tx.send(());
    });
    let observer = std::thread::spawn(move || {
        // Always release the paused event, including on an assertion failure,
        // so the lifecycle owner can finish and the test can report normally.
        let release = scopeguard::guard(release_tx, |sender| {
            let _ = sender.send(());
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.runtime().phase() != LifecyclePhase::Closing {
            assert!(Instant::now() < deadline, "close did not start");
            std::thread::yield_now();
        }
        let late = deliver_events(runtime);
        let unregister_before_release = unregister_rx.recv_timeout(Duration::from_millis(50));
        let still_running = !runtime.runtime().async_runtime().is_stopped();
        drop(release);
        (late, unregister_before_release, still_running)
    });

    // Open and close stay on the same lifecycle thread. The already admitted
    // event is held on a different thread until the observer releases it.
    assert_eq!(host_auto_close(runtime.runtime()), 1);
    event.join().unwrap();
    let (late, unregister_before_release, still_running) = observer.join().unwrap();
    runtime
        .runtime()
        .async_runtime()
        .set_before_calculation_transition_hook(None);
    assert_eq!(late.phase, LifecyclePhase::Closing);
    // Generation publication is sealed before the lifecycle phase changes.
    // Module ingress moves to Closing when the removal claim is acquired, so
    // either projection is valid here; even in that gap the event is rejected.
    assert!(matches!(
        late.ingress_phase,
        crate::ingress::PHASE_OPEN | crate::ingress::PHASE_CLOSING
    ));
    assert_eq!(late.active_exports, 1);
    assert_eq!((late.before, late.after), (before, before));
    assert!(!late.executor_stopped);
    assert!(still_running);
    assert!(matches!(
        unregister_before_release,
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(
        *unregister_state.lock().unwrap(),
        Some((0, before + 1, true)),
        "unregister must observe the admitted event finished and the executor stopped"
    );
    assert_eq!(runtime.runtime().phase(), LifecyclePhase::Closed);
    assert_eq!(runtime.runtime().calculation_id().get(), before + 1);
    assert_rejected(&deliver_events(runtime), LifecyclePhase::Closed);
    let mut calls = opening_calls();
    calls.extend([XLF_UNREGISTER, XLF_EVALUATE]);
    script.assert_calls(&calls);
}
