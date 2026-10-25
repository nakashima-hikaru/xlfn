//! Resident event registrations must not retain a closed generation.

#![allow(
    unsafe_code,
    reason = "The fixture accepts the physical-unload contract for inert application state"
)]

use super::host::{host_auto_close, host_auto_open, host_auto_remove};
use crate::XllError;
use crate::addin::{Addin, OpenContext, Opened, PhysicallyUnloadableAddin};
use crate::diagnostics::AddinId;
use crate::lifecycle::LifecyclePhase;
use crate::registration::host::test_support::{CallbackScript, Reply};
use crate::registration::{RegistrationDescriptor, RegistrationSignature};
use crate::runtime::Runtime;
use std::cell::Cell;
use xlfn_common::{ExecutionKind, FunctionVisibility};
use xlfn_sys::{
    XL_EVENT_REGISTER, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_UNREGISTER,
    XLOPER12, XLOPER12Value, XLRET_FAILED, XLTYPE_STR,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Counts {
    opens: usize,
    quiesces: usize,
    cleanups: usize,
    shared_drops: usize,
    lifecycle_drops: usize,
}

thread_local! {
    static COUNTS: Cell<Counts> = const { Cell::new(Counts {
        opens: 0,
        quiesces: 0,
        cleanups: 0,
        shared_drops: 0,
        lifecycle_drops: 0,
    }) };
}

fn update_counts(update: impl FnOnce(&mut Counts)) {
    let mut counts = COUNTS.get();
    update(&mut counts);
    COUNTS.set(counts);
}

struct SharedState;

impl Drop for SharedState {
    fn drop(&mut self) {
        update_counts(|counts| counts.shared_drops += 1);
    }
}

struct LifecycleState;

impl Drop for LifecycleState {
    fn drop(&mut self) {
        update_counts(|counts| counts.lifecycle_drops += 1);
    }
}

struct ResidentEventAddin;

impl Addin for ResidentEventAddin {
    type SharedState = SharedState;
    type LifecycleState = LifecycleState;
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = crate::NoAsyncExecutor;

    fn open(_: &OpenContext) -> Result<Opened<SharedState, LifecycleState>, XllError> {
        update_counts(|counts| counts.opens += 1);
        Ok(Opened::new(SharedState).with_lifecycle(LifecycleState))
    }

    fn quiesce(_: &mut SharedState, _: &mut LifecycleState) -> Result<(), XllError> {
        update_counts(|counts| counts.quiesces += 1);
        Ok(())
    }

    fn cleanup(_: &mut LifecycleState, _: &mut crate::shutdown::CleanupReporter<'_>) {
        update_counts(|counts| counts.cleanups += 1);
    }
}

// SAFETY: application state owns no threads, callbacks, timers, futures, or
// other executable sources. Host event registration remains framework-owned.
unsafe impl PhysicallyUnloadableAddin for ResidentEventAddin {}

fn runtime(physical_unload: bool) -> Runtime<ResidentEventAddin> {
    COUNTS.set(Counts::default());
    let runtime = if physical_unload {
        Runtime::new_with_physical_unload()
    } else {
        Runtime::new()
    };
    runtime
        .ensure_module_residency(host_auto_close::<ResidentEventAddin> as *const ())
        .unwrap();
    runtime
}

fn open(runtime: &Runtime<ResidentEventAddin>) -> i32 {
    host_auto_open(
        runtime,
        &AddinId::parse("resident-event-test").unwrap(),
        "0",
        "test",
        &[RegistrationDescriptor {
            export_name: "test_resident_event_export",
            excel_name: "TEST.RESIDENT.EVENT",
            signature: RegistrationSignature {
                return_abi: xlfn_common::ReturnAbi::Value12,
                execution: ExecutionKind::MainThread,
                arguments: &[],
                volatile: false,
            },
            category: "Test",
            description: "resident event lifecycle regression",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: &[],
        }],
    )
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

fn expect_reclaimed(generations: usize) {
    assert_eq!(
        COUNTS.get(),
        Counts {
            opens: generations,
            quiesces: generations,
            cleanups: generations,
            shared_drops: generations,
            lifecycle_drops: generations,
        }
    );
}

fn assert_resident_traces(
    runtime: &Runtime<ResidentEventAddin>,
    generations: usize,
    physical_unload: bool,
) {
    let shutdown = runtime.shutdown_trace_json();
    let document: serde_json::Value = serde_json::from_str(&shutdown).unwrap();
    assert_eq!(document["initial"]["residentEventRegistrations"], 1);
    assert_eq!(document["initial"]["moduleResidencyHeld"], true);
    assert_eq!(document["outcome"], "returned_success");
    assert!(!shutdown.contains("unregisterEvent"));

    let composition = runtime.composition_trace_json();
    let document: serde_json::Value = serde_json::from_str(&composition).unwrap();
    assert_eq!(document["outcome"], "returned_success");
    assert!(!composition.contains("unregisterEvent"));
    let events = document["events"].as_array().unwrap();
    let committed = events
        .iter()
        .filter_map(|event| event.get("commitOpen"))
        .collect::<Vec<_>>();
    assert_eq!(committed.len(), generations);
    for opened in committed {
        assert_eq!(opened["resources"]["residentEventRegistrations"], 1);
        assert_eq!(opened["resources"]["moduleResidencyHeld"], true);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event.as_str() == Some("publishCommittedClosed"))
            .count(),
        generations
    );

    let policy = if physical_unload {
        "physical"
    } else {
        "logical"
    };
    for (kind, checker_variable, trace) in [
        ("shutdown", "XLFN_SHUTDOWN_CHECKER", shutdown),
        ("composition", "XLFN_COMPOSITION_CHECKER", composition),
    ] {
        let label = format!("resident-events-{policy}-{generations}-{kind}");
        if let Some(directory) = std::env::var_os("XLFN_COMPOSITION_TRACE_DIR") {
            let path = std::path::Path::new(&directory).join(format!("rust-{label}.json"));
            std::fs::write(path, &trace).expect("write resident-event Rust trace");
        }
        let Some(checker) = std::env::var_os(checker_variable) else {
            continue;
        };
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new(checker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("failed to start {checker_variable}: {error}"));
        child
            .stdin
            .take()
            .expect("Lean checker stdin is piped")
            .write_all(trace.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Lean checker rejected {label}: {}\n{}\n{trace}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn close_and_reopen_reuse_resident_events_and_reclaim_each_generation() {
    for physical_unload in [false, true] {
        let script = CallbackScript::install([
            Reply::success(XL_GET_NAME, module_name()),
            Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(123)),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
            Reply::success(XLF_REGISTER, XLOPER12::number(7.0)),
            Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
            Reply::success(XL_GET_NAME, module_name()),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
            Reply::success(XLF_REGISTER, XLOPER12::number(9.0)),
            Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
        ]);
        let runtime = runtime(physical_unload);

        assert_eq!(open(&runtime), 1);
        let first_generation = runtime.last_committed_generation();
        assert_eq!(host_auto_remove(&runtime), 1);
        assert_eq!(host_auto_close(&runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        expect_reclaimed(1);
        assert_eq!(runtime.resident_event_count(), 1);
        assert!(runtime.module_residency_held());
        assert!(!runtime.release_module_residency().unwrap());
        assert!(runtime.module_residency_held());
        assert_resident_traces(&runtime, 1, physical_unload);

        assert_eq!(open(&runtime), 1);
        assert!(runtime.last_committed_generation() > first_generation);
        assert_eq!(host_auto_close(&runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        expect_reclaimed(2);
        assert_eq!(runtime.resident_event_count(), 1);
        assert!(runtime.module_residency_held());
        assert_resident_traces(&runtime, 2, physical_unload);

        // The sole event call belongs to first installation: neither close
        // nor reopen may attempt nil removal or add a second subscription.
        script.assert_calls(&[
            XL_GET_NAME,
            XL_EVENT_REGISTER,
            XLF_EVALUATE,
            XLF_REGISTER,
            XLF_UNREGISTER,
            XLF_EVALUATE,
            XL_GET_NAME,
            XLF_EVALUATE,
            XLF_REGISTER,
            XLF_UNREGISTER,
            XLF_EVALUATE,
        ]);
    }
}

#[test]
fn failed_function_registration_retains_event_for_retry_without_retaining_state() {
    for physical_unload in [false, true] {
        let script = CallbackScript::install([
            Reply::success(XL_GET_NAME, module_name()),
            Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(123)),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
            Reply::success(XLF_REGISTER, XLOPER12::nil()).status(XLRET_FAILED),
            Reply::success(XL_GET_NAME, module_name()),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
            Reply::success(XLF_REGISTER, XLOPER12::number(9.0)),
            Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
            Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
        ]);
        let runtime = runtime(physical_unload);

        assert_eq!(open(&runtime), 0);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        expect_reclaimed(1);
        assert_eq!(runtime.resident_event_count(), 1);
        assert!(runtime.module_residency_held());
        assert!(!runtime.release_module_residency().unwrap());
        assert!(runtime.module_residency_held());

        assert_eq!(open(&runtime), 1);
        assert_eq!(host_auto_close(&runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        expect_reclaimed(2);
        assert_eq!(runtime.resident_event_count(), 1);
        assert!(runtime.module_residency_held());
        script.assert_calls(&[
            XL_GET_NAME,
            XL_EVENT_REGISTER,
            XLF_EVALUATE,
            XLF_REGISTER,
            XL_GET_NAME,
            XLF_EVALUATE,
            XLF_REGISTER,
            XLF_UNREGISTER,
            XLF_EVALUATE,
        ]);
    }
}
