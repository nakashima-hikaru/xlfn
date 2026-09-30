#![allow(
    unsafe_code,
    reason = "The lifecycle fixture accepts the physical-unload contract for empty application state"
)]

use super::{host_auto_close, host_auto_open};
use crate::XllError;
use crate::addin::{Addin, OpenContext, Opened, PhysicallyUnloadableAddin};
use crate::diagnostics::AddinId;
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use crate::registration::host::test_support::{CallbackScript, Reply};
use crate::registration::{
    HostMutationJournal, RegistrationDescriptor, RegistrationId, RegistrationSignature,
};
use crate::runtime::Runtime;
use std::cell::Cell;
use xlfn_common::{ExecutionKind, FunctionVisibility};
use xlfn_sys::{
    XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME, XLF_UNREGISTER, XLOPER12,
    XLOPER12Value, XLRET_ABORT, XLRET_UNCALCED, XLTYPE_STR,
};

struct ReloadAddin;

thread_local! {
    static OPENS: Cell<usize> = const { Cell::new(0) };
    static QUIESCES: Cell<usize> = const { Cell::new(0) };
}

impl Addin for ReloadAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
        OPENS.set(OPENS.get() + 1);
        Ok(Opened::new(()))
    }

    fn quiesce(_: &mut (), _: &mut ()) -> Result<(), XllError> {
        QUIESCES.set(QUIESCES.get() + 1);
        Ok(())
    }
}

// SAFETY: this fixture owns only () and creates no application threads,
// callbacks, timers, or other sources that can execute after quiescence.
unsafe impl PhysicallyUnloadableAddin for ReloadAddin {}

fn descriptors() -> [RegistrationDescriptor; 1] {
    [RegistrationDescriptor {
        export_name: "test_terminal_reload_export",
        excel_name: "TEST.TERMINAL.RELOAD",
        signature: RegistrationSignature {
            execution: ExecutionKind::MainThread,
            arguments: &[],
            volatile: false,
        },
        category: "Test",
        description: "terminal lifecycle reload test",
        help_topic: "",
        visibility: FunctionVisibility::Public,
        arguments: &[],
    }]
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

fn publish_old_generation() -> Runtime<ReloadAddin> {
    OPENS.set(0);
    QUIESCES.set(0);
    let runtime = Runtime::<ReloadAddin>::new_with_physical_unload();
    runtime
        .ensure_module_residency(host_auto_close::<ReloadAddin> as *const ())
        .unwrap();
    let opening = runtime.begin_open().unwrap();
    let mut opening = runtime.publish(opening, (), ());
    runtime
        .finish_open(
            &mut opening,
            vec![RegistrationId {
                id: 7.0,
                excel_name: "TEST.TERMINAL.RELOAD",
            }],
        )
        .unwrap();
    drop(opening);
    runtime
}

fn open(runtime: &Runtime<ReloadAddin>) -> i32 {
    host_auto_open(
        runtime,
        &AddinId::parse("terminal-reload").unwrap(),
        "0",
        "test",
        &descriptors(),
    )
}

fn replacement_replies() -> [Reply; 3] {
    [
        Reply::success(XL_GET_NAME, module_name()),
        Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
        Reply::success(XLF_REGISTER, XLOPER12::number(9.0)),
    ]
}

fn close_replacement(runtime: &Runtime<ReloadAddin>) {
    let previous_quiesces = QUIESCES.get();
    let script = CallbackScript::install([
        Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
        Reply::success(XLF_EVALUATE, XLOPER12::number(9.0)),
        Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
    ]);
    assert_eq!(host_auto_close(runtime), 1);
    assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
    assert!(!runtime.module_residency_held());
    assert_eq!(runtime.metadata_debt_count_for_test(), 0);
    assert_eq!((OPENS.get(), QUIESCES.get()), (1, previous_quiesces + 1));
    script.assert_calls(&[XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME]);
}

#[test]
fn reload_stops_after_terminal_metadata_cleanup_and_retries_debt_on_later_entry() {
    for status in [XLRET_ABORT, XLRET_UNCALCED] {
        for terminal_during_free in [false, true] {
            let evaluate = Reply::success(XLF_EVALUATE, XLOPER12::number(7.0));
            let evaluate = if terminal_during_free {
                evaluate.release_status(status)
            } else {
                evaluate.status(status)
            };
            let script = CallbackScript::install([
                Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
                evaluate,
            ]);
            // Publish exactly one synchronous UDF. No async event callback
            // remains to turn the terminal metadata error into quarantine.
            let runtime = publish_old_generation();
            let old_generation = runtime.last_committed_generation();

            assert_eq!(open(&runtime), 0);
            assert_eq!(runtime.phase(), LifecyclePhase::Closed);
            assert_eq!(
                runtime.host_intent(),
                HostLifecycleIntent::ExplicitRemovalComplete,
            );
            assert!(runtime.module_residency_held());
            assert_eq!(runtime.last_committed_generation(), old_generation);
            assert_eq!(runtime.metadata_debt_count_for_test(), 1);
            assert_eq!((OPENS.get(), QUIESCES.get()), (0, 1));
            // The fixture checks every raw callback, with xlFree allowed
            // exactly once for each result. No new xlGetName, registration,
            // or metadata retry is allowed in this terminal invocation.
            script.assert_calls(&[XLF_UNREGISTER, XLF_EVALUATE]);
            drop(script);

            let retry = CallbackScript::install(
                [
                    Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                    Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
                ]
                .into_iter()
                .chain(replacement_replies()),
            );
            assert_eq!(open(&runtime), 1);
            assert_eq!(runtime.phase(), LifecyclePhase::Open);
            assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
            assert!(runtime.module_residency_held());
            assert!(runtime.last_committed_generation() > old_generation);
            assert_eq!(runtime.metadata_debt_count_for_test(), 0);
            assert_eq!((OPENS.get(), QUIESCES.get()), (1, 1));
            retry.assert_calls(&[
                XLF_EVALUATE,
                XLF_SET_NAME,
                XL_GET_NAME,
                XLF_EVALUATE,
                XLF_REGISTER,
            ]);
            drop(retry);
            close_replacement(&runtime);
        }
    }
}

#[test]
fn pending_rollback_terminal_cleanup_stops_before_a_replacement_open() {
    for status in [XLRET_ABORT, XLRET_UNCALCED] {
        for terminal_during_free in [false, true] {
            let _callback_guard = crate::test_callback::lock();
            OPENS.set(0);
            QUIESCES.set(0);
            let runtime = Runtime::<ReloadAddin>::new_with_physical_unload();
            runtime
                .ensure_module_residency(host_auto_close::<ReloadAddin> as *const ())
                .unwrap();
            let opening = runtime.begin_open().unwrap();
            assert!(opening.fail_for_test().requires_rollback());
            assert_eq!(runtime.phase(), LifecyclePhase::OpenRollbackPending);
            runtime.merge_host_for_test(HostMutationJournal {
                pending_registrations: vec![
                    RegistrationId {
                        id: 7.0,
                        excel_name: "TEST.TERMINAL.RELOAD",
                    }
                    .into(),
                ],
                ..HostMutationJournal::default()
            });
            let evaluate = Reply::success(XLF_EVALUATE, XLOPER12::number(7.0));
            let evaluate = if terminal_during_free {
                evaluate.release_status(status)
            } else {
                evaluate.status(status)
            };
            let rollback = CallbackScript::install([
                Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
                evaluate,
            ]);

            assert_eq!(open(&runtime), 0);
            assert_eq!(runtime.phase(), LifecyclePhase::Closed);
            assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
            assert!(runtime.module_residency_held());
            assert_eq!(runtime.metadata_debt_count_for_test(), 1);
            assert_eq!((OPENS.get(), QUIESCES.get()), (0, 0));
            rollback.assert_calls(&[XLF_UNREGISTER, XLF_EVALUATE]);
            drop(rollback);

            let retry = CallbackScript::install(
                [
                    Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                    Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
                ]
                .into_iter()
                .chain(replacement_replies()),
            );
            assert_eq!(open(&runtime), 1);
            assert_eq!(runtime.phase(), LifecyclePhase::Open);
            assert_eq!(runtime.metadata_debt_count_for_test(), 0);
            assert_eq!((OPENS.get(), QUIESCES.get()), (1, 0));
            retry.assert_calls(&[
                XLF_EVALUATE,
                XLF_SET_NAME,
                XL_GET_NAME,
                XLF_EVALUATE,
                XLF_REGISTER,
            ]);
            drop(retry);
            close_replacement(&runtime);
        }
    }
}

#[test]
fn reload_stops_after_terminal_free_of_successful_final_name_deletion() {
    for status in [XLRET_ABORT, XLRET_UNCALCED] {
        let script = CallbackScript::install([
            Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
            Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
            Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)).release_status(status),
        ]);
        let runtime = publish_old_generation();
        let old_generation = runtime.last_committed_generation();

        assert_eq!(open(&runtime), 0);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert_eq!(
            runtime.host_intent(),
            HostLifecycleIntent::ExplicitRemovalComplete,
        );
        assert!(runtime.module_residency_held());
        assert_eq!(runtime.last_committed_generation(), old_generation);
        // Deletion committed before its result's xlFree failed, so the name
        // must stay resolved rather than being converted into metadata debt.
        assert_eq!(runtime.metadata_debt_count_for_test(), 0);
        assert_eq!((OPENS.get(), QUIESCES.get()), (0, 1));
        script.assert_calls(&[XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME]);
        drop(script);

        let close = CallbackScript::install([]);
        assert_eq!(host_auto_close(&runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
        assert!(!runtime.module_residency_held());
        assert_eq!((OPENS.get(), QUIESCES.get()), (0, 1));
        close.assert_calls(&[]);
    }
}

#[test]
fn nonterminal_reload_opens_replacement_and_later_close_releases_residency() {
    let script = CallbackScript::install(
        [
            Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
            Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
            Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
        ]
        .into_iter()
        .chain(replacement_replies()),
    );
    let runtime = publish_old_generation();
    let old_generation = runtime.last_committed_generation();

    assert_eq!(open(&runtime), 1);
    assert_eq!(runtime.phase(), LifecyclePhase::Open);
    assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
    assert!(runtime.module_residency_held());
    assert!(runtime.last_committed_generation() > old_generation);
    assert_eq!(runtime.metadata_debt_count_for_test(), 0);
    assert_eq!((OPENS.get(), QUIESCES.get()), (1, 1));
    script.assert_calls(&[
        XLF_UNREGISTER,
        XLF_EVALUATE,
        XLF_SET_NAME,
        XL_GET_NAME,
        XLF_EVALUATE,
        XLF_REGISTER,
    ]);
    drop(script);
    close_replacement(&runtime);
}
