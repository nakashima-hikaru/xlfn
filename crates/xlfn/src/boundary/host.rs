//! Excel lifecycle callback adapters.
//!
//! These functions are the only host-boundary entry points that translate
//! Excel callbacks into the runtime protocol. They live beside the lifecycle
//! domain but own host-facing policy rather than canonical state transitions.

use crate::addin::Addin;
use crate::boundary::report_boundary_error;
use crate::diagnostics::AddinId;
use crate::host_callback::HostCallbackSession;
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase, lifecycle_access_error};
use crate::registration::RegistrationDescriptor;
use crate::runtime::Runtime;
use std::cell::RefCell;

thread_local! {
    static ACTIVE_LIFECYCLES: RefCell<Vec<*const ()>> = const { RefCell::new(Vec::new()) };
}

/// Excel callbacks made by open/cleanup can reenter a lifecycle export. The
/// outer boundary already owns the transition; waiting for it on this thread
/// would deadlock. Coalesce nested deactivation into that operation without
/// leaving a removal intent for its replacement generation. Track runtime
/// identity so another add-in is still handled.
struct HostLifecycleGuard {
    runtime: *const (),
}

impl HostLifecycleGuard {
    fn enter<A: Addin>(runtime: &Runtime<A>) -> Option<Self> {
        let runtime = std::ptr::from_ref(runtime).cast::<()>();
        ACTIVE_LIFECYCLES.with_borrow_mut(|active| {
            if active.contains(&runtime) {
                None
            } else {
                active.push(runtime);
                Some(Self { runtime })
            }
        })
    }
}

impl Drop for HostLifecycleGuard {
    fn drop(&mut self) {
        ACTIVE_LIFECYCLES.with_borrow_mut(|active| {
            assert_eq!(active.pop(), Some(self.runtime));
        });
    }
}

/// Handles the generated `xlAutoOpen` boundary.
pub(crate) fn host_auto_open<A>(
    runtime: &Runtime<A>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    let Some(_boundary) = HostLifecycleGuard::enter(runtime) else {
        return 0;
    };
    // One Excel invocation owns one terminal callback state, including a
    // controlled removal and its replacement open transaction.
    let mut callbacks = HostCallbackSession::new();
    if runtime.phase() == LifecyclePhase::Quarantined {
        return 0;
    }
    let mut lifecycle = match runtime.bind_addin_lifecycle() {
        Ok(access) => access,
        Err(error) => {
            let error = lifecycle_access_error(error);
            report_boundary_error("xlAutoOpen lifecycle thread", &error);
            runtime.quarantine_runtime();
            return 0;
        }
    };
    let controlled_reload = runtime.phase() == crate::lifecycle::LifecyclePhase::Open;
    let removal_completed_before_open = runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
        && runtime.host_intent() == HostLifecycleIntent::ExplicitRemovalComplete;
    if controlled_reload {
        let result = runtime.remove_addin(&lifecycle, &mut callbacks);
        if result == 0 || runtime.phase() != crate::lifecycle::LifecyclePhase::Closed {
            return 0;
        }
        if !callbacks.permits_callbacks() {
            // Detaching the old generation may succeed despite metadata or
            // xlFree failures. Abort/Uncalced still require returning to Excel
            // before any replacement callbacks. Preserve the completed-removal
            // marker for a later close or a new Excel open invocation.
            runtime.complete_explicit_removal();
            return 0;
        }
        lifecycle = match runtime.bind_addin_lifecycle() {
            Ok(access) => access,
            Err(error) => {
                let error = lifecycle_access_error(error);
                report_boundary_error("xlAutoOpen lifecycle rebind", &error);
                runtime.quarantine_runtime();
                return 0;
            }
        };
        runtime.clear_host_intent();
    }
    let result = runtime.open_addin_boundary(
        &lifecycle,
        callbacks,
        addin_id,
        version,
        target,
        descriptors,
    );
    if controlled_reload
        && result == 0
        && runtime.phase() != crate::lifecycle::LifecyclePhase::Quarantined
    {
        // A reload has already destroyed the previous generation. A failed
        // replacement must therefore not leave a closed runtime with the old
        // residency lease and no generation owner.
        runtime.quarantine_runtime();
    } else if result == 0
        && removal_completed_before_open
        && runtime.phase() == crate::lifecycle::LifecyclePhase::Closed
    {
        // The old generation was already removed successfully, but Excel
        // attempted a new open before delivering xlAutoClose. Preserve the
        // release marker so that the later close can release the lease.
        runtime.complete_explicit_removal();
    }
    result
}

/// Deactivates the add-in, including host paths that do not call xlAutoRemove.
pub(crate) fn host_auto_close<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    let Some(_boundary) = HostLifecycleGuard::enter(runtime) else {
        return 1;
    };
    let mut callbacks = HostCallbackSession::new();
    deactivate_addin(runtime, &mut callbacks, "xlAutoClose lifecycle thread");
    if runtime.phase() == LifecyclePhase::Closed
        && runtime.host_intent() == HostLifecycleIntent::ExplicitRemovalComplete
    {
        if runtime.physical_unload_enabled() {
            if let Err(error) = runtime.release_module_residency() {
                report_boundary_error("xlAutoClose module residency release", &error);
                runtime.quarantine_runtime();
            } else {
                runtime.clear_host_intent();
            }
        } else {
            // A safe Addin may own executable sources outside framework
            // accounting. Keep the DLL resident unless the Addin explicitly
            // accepted the physical-unload contract.
            runtime.clear_host_intent();
        }
    }
    1
}

/// Handles the explicit terminal-removal boundary.
pub(crate) fn host_auto_remove<A>(runtime: &Runtime<A>) -> i32
where
    A: Addin,
{
    let Some(_boundary) = HostLifecycleGuard::enter(runtime) else {
        return 1;
    };
    let mut callbacks = HostCallbackSession::new();
    deactivate_addin(runtime, &mut callbacks, "xlAutoRemove lifecycle thread");
    1
}

fn deactivate_addin<A: Addin>(
    runtime: &Runtime<A>,
    callbacks: &mut HostCallbackSession,
    boundary: &'static str,
) {
    if runtime.phase() == LifecyclePhase::Quarantined {
        return;
    }
    let lifecycle = match runtime.bind_addin_lifecycle() {
        Ok(access) => access,
        Err(error) => {
            let error = lifecycle_access_error(error);
            report_boundary_error(boundary, &error);
            runtime.quarantine_runtime();
            return;
        }
    };
    runtime.request_explicit_removal();
    let result = runtime.remove_addin(&lifecycle, callbacks);
    if result == 1 && runtime.phase() == LifecyclePhase::Closed {
        runtime.complete_explicit_removal();
    }
}

#[cfg(all(
    test,
    any(not(target_os = "windows"), feature = "async", feature = "handles")
))]
#[path = "host_terminal_tests.rs"]
mod terminal_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::XllError;
    use crate::addin::{OpenContext, Opened};
    use std::sync::{Arc, Mutex};

    struct ReentrantClose;

    struct CloseState {
        runtime: &'static Runtime<ReentrantClose>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl CloseState {
        fn reenter(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
            let intent = self.runtime.host_intent();
            let epoch = self.runtime.removal_epoch();
            assert_eq!(host_auto_close(self.runtime), 1);
            assert_eq!(host_auto_remove(self.runtime), 1);
            assert_eq!(self.runtime.host_intent(), intent);
            assert_eq!(self.runtime.removal_epoch(), epoch);
            assert_eq!(self.runtime.phase(), LifecyclePhase::Closing);
            assert!(self.runtime.module_residency_held());
        }
    }

    impl Drop for CloseState {
        fn drop(&mut self) {
            self.reenter("drop");
        }
    }

    impl Addin for ReentrantClose {
        type SharedState = ();
        type LifecycleState = CloseState;
        type Error = XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &OpenContext) -> Result<Opened<(), CloseState>, XllError> {
            unreachable!("test publishes lifecycle state directly")
        }

        fn quiesce(_: &mut (), state: &mut CloseState) -> Result<(), XllError> {
            state.reenter("quiesce");
            assert_eq!(
                host_auto_open(
                    state.runtime,
                    &AddinId::parse("test").unwrap(),
                    "0",
                    "test",
                    &[]
                ),
                0
            );
            Ok(())
        }

        fn cleanup(state: &mut CloseState, _: &mut crate::shutdown::CleanupReporter<'_>) {
            state.reenter("cleanup");
        }
    }

    #[test]
    fn close_coalesces_reentrant_lifecycle_calls_through_state_destruction() {
        let fixture = crate::runtime::StaticTestRuntime::<ReentrantClose>::new();
        let runtime = fixture.runtime();
        runtime
            .ensure_module_residency(host_auto_close::<ReentrantClose> as *const ())
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish_with_lifecycle(
            opening,
            (),
            CloseState {
                runtime,
                events: Arc::clone(&events),
            },
            (),
        );
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        assert_eq!(host_auto_close(runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert_eq!(*events.lock().unwrap(), ["quiesce", "cleanup", "drop"]);
        assert!(runtime.module_residency_held());
    }

    #[test]
    fn nested_runtime_does_not_hide_the_outer_lifecycle_owner() {
        let outer = Runtime::<()>::new();
        let inner = Runtime::<()>::new();
        let epoch = outer.removal_epoch();
        let outer_guard = HostLifecycleGuard::enter(&outer).unwrap();
        let inner_guard = HostLifecycleGuard::enter(&inner).unwrap();
        assert_eq!(host_auto_close(&outer), 1);
        assert_eq!(host_auto_remove(&outer), 1);
        assert_eq!(outer.removal_epoch(), epoch);
        drop(inner_guard);
        drop(outer_guard);

        assert_eq!(host_auto_close(&outer), 1);
        assert!(outer.begin_open_if_epoch(epoch).is_err());
    }

    #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
    #[allow(
        unsafe_code,
        reason = "Test callback exercises the audited Excel lifecycle ABI"
    )]
    mod reload_callback_tests {
        use super::*;
        use crate::addin::PhysicallyUnloadableAddin;
        use crate::registration::RegistrationSignature;
        use xlfn_common::{ExecutionKind, FunctionVisibility};
        use xlfn_sys::{
            XL_FREE, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_UNREGISTER, XLOPER12,
            XLOPER12Value, XLRET_FAILED, XLRET_SUCCESS, XLTYPE_STR,
        };

        struct ReloadAddin;

        impl Addin for ReloadAddin {
            type SharedState = ();
            type LifecycleState = ();
            type Error = XllError;
            type Layers = ();
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;

            fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
                SCRIPT.with_borrow_mut(|script| script.opens += 1);
                Ok(Opened::new(()))
            }

            fn quiesce(_: &mut (), _: &mut ()) -> Result<(), XllError> {
                SCRIPT.with_borrow_mut(|script| script.quiesces += 1);
                Ok(())
            }
        }

        // SAFETY: this test add-in creates no threads, callbacks, or other
        // application execution sources; its state consists only of ().
        unsafe impl PhysicallyUnloadableAddin for ReloadAddin {}

        #[derive(Default)]
        struct ReloadScript {
            runtime: *const Runtime<ReloadAddin>,
            calls: Vec<i32>,
            opens: usize,
            quiesces: usize,
            registrations: usize,
            reenter_on_unregister: bool,
            reentries: usize,
            reentry_preserved_owner: bool,
        }

        thread_local! {
            static SCRIPT: RefCell<ReloadScript> = RefCell::new(ReloadScript::default());
        }

        struct ReloadCallbacks<'runtime> {
            _runtime: &'runtime Runtime<ReloadAddin>,
            _guard: crate::test_callback::CallbackTestGuard,
        }

        impl<'runtime> ReloadCallbacks<'runtime> {
            fn install(runtime: &'runtime Runtime<ReloadAddin>) -> Self {
                let guard = crate::test_callback::lock();
                crate::module_runtime::reset_callbacks_for_test();
                SCRIPT.with_borrow_mut(|script| {
                    *script = ReloadScript {
                        runtime: std::ptr::from_ref(runtime),
                        ..ReloadScript::default()
                    };
                });
                // SAFETY: callback has Excel's exact ABI and is process-live;
                // the shared test guard excludes other callback fixtures.
                unsafe {
                    xlfn_sys::install_callback_for_abi_probe(
                        callback as *const () as *mut std::ffi::c_void,
                    );
                }
                Self {
                    _runtime: runtime,
                    _guard: guard,
                }
            }
        }

        impl Drop for ReloadCallbacks<'_> {
            fn drop(&mut self) {
                crate::test_callback::install();
                crate::module_runtime::reset_callbacks_for_test();
                SCRIPT.with_borrow_mut(|script| *script = ReloadScript::default());
            }
        }

        unsafe extern "system" fn callback(
            function: i32,
            _argument_count: i32,
            _arguments: *mut *mut XLOPER12,
            result: *mut XLOPER12,
        ) -> i32 {
            let reentry = SCRIPT.with_borrow_mut(|script| {
                script.calls.push(function);
                if function == XLF_UNREGISTER && script.reenter_on_unregister {
                    script.reenter_on_unregister = false;
                    Some(script.runtime)
                } else {
                    None
                }
            });
            if let Some(runtime) = reentry {
                // SAFETY: ReloadCallbacks retains the runtime borrow until
                // after this callback is uninstalled, on the same thread.
                let runtime = unsafe { &*runtime };
                let intent = runtime.host_intent();
                let epoch = runtime.removal_epoch();
                let status = host_auto_close(runtime);
                let preserved = status == 1
                    && runtime.host_intent() == intent
                    && runtime.removal_epoch() == epoch
                    && runtime.phase() == LifecyclePhase::Closing
                    && runtime.module_residency_held();
                SCRIPT.with_borrow_mut(|script| {
                    script.reentries += 1;
                    script.reentry_preserved_owner = preserved;
                });
            }
            if function == XL_FREE {
                return XLRET_SUCCESS;
            }
            if result.is_null() {
                return XLRET_FAILED;
            }
            let value = match function {
                XL_GET_NAME => {
                    #[cfg(not(target_os = "windows"))]
                    static MODULE_NAME: [u16; 10] = [9, 47, 116, 101, 115, 116, 46, 120, 108, 108];
                    #[cfg(target_os = "windows")]
                    static MODULE_NAME: [u16; 9] = [8, 67, 58, 92, 116, 46, 120, 108, 108];
                    XLOPER12 {
                        value: XLOPER12Value {
                            string: MODULE_NAME.as_ptr().cast_mut(),
                        },
                        xltype: XLTYPE_STR,
                    }
                }
                XLF_EVALUATE => XLOPER12::error(XLERR_NAME),
                XLF_REGISTER => {
                    let id = SCRIPT.with_borrow_mut(|script| {
                        script.registrations += 1;
                        script.registrations
                    });
                    XLOPER12::number(id as f64)
                }
                XLF_UNREGISTER => XLOPER12::boolean(true),
                _ => return XLRET_FAILED,
            };
            // SAFETY: Excel's callback contract supplies writable result
            // storage; any referenced test data remains process-live.
            unsafe { *result = value };
            XLRET_SUCCESS
        }

        #[test]
        fn controlled_reload_coalesces_close_reentered_from_unregister() {
            let runtime = Runtime::<ReloadAddin>::new_with_physical_unload();
            let _callbacks = ReloadCallbacks::install(&runtime);
            runtime
                .ensure_module_residency(host_auto_close::<ReloadAddin> as *const ())
                .unwrap();
            let descriptors = [RegistrationDescriptor {
                export_name: "test_reload_export",
                excel_name: "TEST.RELOAD.EXPORT",
                signature: RegistrationSignature {
                    execution: ExecutionKind::MainThread,
                    arguments: &[],
                    volatile: false,
                },
                category: "Test",
                description: "test reload",
                help_topic: "",
                visibility: FunctionVisibility::Public,
                arguments: &[],
            }];
            let addin_id = AddinId::parse("reload").unwrap();
            assert_eq!(
                host_auto_open(&runtime, &addin_id, "0", "test", &descriptors),
                1
            );
            let first_generation = runtime.last_committed_generation();
            SCRIPT.with_borrow_mut(|script| script.reenter_on_unregister = true);

            assert_eq!(
                host_auto_open(&runtime, &addin_id, "0", "test", &descriptors),
                1
            );
            assert_eq!(runtime.phase(), LifecyclePhase::Open);
            assert!(runtime.last_committed_generation() > first_generation);
            assert_eq!(runtime.host_intent(), HostLifecycleIntent::None);
            assert!(runtime.module_residency_held());
            SCRIPT.with_borrow(|script| {
                assert_eq!(script.reentries, 1);
                assert!(script.reentry_preserved_owner);
                assert_eq!(script.opens, 2);
                assert_eq!(script.quiesces, 1);
            });

            assert_eq!(host_auto_close(&runtime), 1);
            assert_eq!(runtime.phase(), LifecyclePhase::Closed);
            assert!(!runtime.module_residency_held());
            SCRIPT.with_borrow(|script| {
                assert_eq!(script.quiesces, 2);
                assert_eq!(script.registrations, 2);
                let calls = script
                    .calls
                    .iter()
                    .copied()
                    .filter(|function| *function != XL_FREE)
                    .collect::<Vec<_>>();
                assert_eq!(
                    calls,
                    [
                        XL_GET_NAME,
                        XLF_EVALUATE,
                        XLF_REGISTER,
                        XLF_UNREGISTER,
                        XLF_EVALUATE,
                        XL_GET_NAME,
                        XLF_EVALUATE,
                        XLF_REGISTER,
                        XLF_UNREGISTER,
                        XLF_EVALUATE,
                    ]
                );
                assert_eq!(
                    script.calls.iter().filter(|call| **call == XL_FREE).count(),
                    calls.len()
                );
            });
        }
    }
}
