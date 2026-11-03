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
use std::{cell::Cell, marker::PhantomPinned, pin::pin, ptr};

thread_local! {
    static ACTIVE_HEAD: Cell<*const LifecycleFrame> = const { Cell::new(ptr::null()) };
}

/// Stack nodes remain pinned until the private registration is popped. Foreign
/// nonlocal jumps across Rust frames are not supported: they bypass Drop and
/// would leave dangling TLS links. Ordinary returns and Rust unwinds are safe.
struct LifecycleFrame {
    runtime: *const (),
    previous: *const LifecycleFrame,
    _pin: PhantomPinned,
}

struct LifecycleRegistration<'a> {
    frame: &'a LifecycleFrame,
}
impl Drop for LifecycleRegistration<'_> {
    fn drop(&mut self) {
        ACTIVE_HEAD.with(|head| {
            if !ptr::eq(head.get(), ptr::from_ref(self.frame)) {
                // Restoring an unexpected head would conceal a broken chain.
                std::process::abort();
            }
            head.set(self.frame.previous);
        });
    }
}

/// Search links only while all enclosing scoped registrations are alive.
#[allow(
    unsafe_code,
    reason = "Pinned stack nodes are scoped and popped in strict LIFO order"
)]
fn with_host_lifecycle<R>(
    runtime: *const (),
    operation: impl for<'scope> FnOnce(&'scope LifecycleFrame) -> R,
) -> Option<R> {
    let previous = ACTIVE_HEAD.with(|head| {
        let mut cursor = head.get();
        while !cursor.is_null() {
            // SAFETY: only this function publishes pinned local frames, and
            // its private registration removes each one before the frame dies.
            // All previous pointers refer to live outer invocations on this thread.
            let frame = unsafe { &*cursor };
            if frame.runtime == runtime {
                return None;
            }
            cursor = frame.previous;
        }
        Some(head.get())
    })?;
    let pinned = pin!(LifecycleFrame {
        runtime,
        previous,
        _pin: PhantomPinned
    });
    let frame = pinned.as_ref().get_ref();
    ACTIVE_HEAD.with(|head| head.set(ptr::from_ref(frame)));
    // No user code or fallible operation may run between publication and
    // construction. Registration is private and never given to the callback.
    let _registration = LifecycleRegistration { frame };
    Some(operation(frame))
}

/// Close authority is borrowed from this exact active host invocation.
pub(crate) struct HostCloseBoundary<'scope> {
    frame: &'scope LifecycleFrame,
}
impl HostCloseBoundary<'_> {
    pub(crate) fn belongs_to<A: Addin>(
        &self,
        runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    ) -> bool {
        self.frame.runtime == ptr::from_ref(runtime).cast::<()>()
    }
}

#[cfg(test)]
pub(crate) fn with_test_close_boundary<A: Addin, R>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    operation: impl for<'scope> FnOnce(HostCloseBoundary<'scope>) -> R,
) -> R {
    with_host_lifecycle(ptr::from_ref(runtime).cast::<()>(), |frame| {
        operation(HostCloseBoundary { frame })
    })
    .expect("test owns its close boundary")
}

fn defer_opening_deactivation<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
) {
    if runtime.phase() == LifecyclePhase::Opening {
        // Do not wait for our own opening transaction or take its cleanup
        // authority. It will observe this intent after returning from the
        // current host/application callback and roll back with its session.
        runtime.request_explicit_removal();
    }
}

/// Handles the generated `xlAutoOpen` boundary.
#[cfg(test)]
pub(crate) fn host_auto_open<A>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> i32
where
    A: Addin,
{
    host_auto_open_observed(runtime, addin_id, version, target, descriptors).result()
}

/// Records the attempt claimed by this exact opening invocation.
pub(crate) struct HostOpenOutcome {
    result: i32,
    attempt: Option<crate::generation::OpenAttemptId>,
}

impl HostOpenOutcome {
    pub(crate) fn result(&self) -> i32 {
        self.result
    }
    pub(crate) fn attempt(&self) -> Option<crate::generation::OpenAttemptId> {
        self.attempt
    }
}

pub(crate) fn host_auto_open_observed<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
) -> HostOpenOutcome {
    let mut attempted = None;
    let result = host_auto_open_inner(
        runtime,
        addin_id,
        version,
        target,
        descriptors,
        &mut attempted,
    );
    HostOpenOutcome {
        result,
        attempt: attempted,
    }
}

fn host_auto_open_inner<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    addin_id: &AddinId,
    version: &'static str,
    target: &'static str,
    descriptors: &[RegistrationDescriptor],
    attempted: &mut Option<crate::generation::OpenAttemptId>,
) -> i32 {
    with_host_lifecycle(ptr::from_ref(runtime).cast::<()>(), |_| {
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
        let removal_completed_before_open = runtime.phase()
            == crate::lifecycle::LifecyclePhase::Closed
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
            crate::runtime::OpenBoundaryRequest {
                addin_id,
                version,
                target,
                descriptors,
            },
            attempted,
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
    })
    .unwrap_or(0)
}

/// Deactivates the add-in, including host paths that do not call xlAutoRemove.
pub(crate) fn host_auto_close<A>(runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>) -> i32
where
    A: Addin,
{
    with_host_lifecycle(ptr::from_ref(runtime).cast::<()>(), |frame| {
        let boundary = HostCloseBoundary { frame };
        let mut callbacks = HostCallbackSession::new();
        deactivate_addin(runtime, &mut callbacks, "xlAutoClose lifecycle thread");
        if runtime.phase() == LifecyclePhase::Closed
            && runtime.host_intent() == HostLifecycleIntent::ExplicitRemovalComplete
        {
            if runtime.physical_unload_enabled() {
                if let Err(error) = runtime.release_closed_residency(&boundary) {
                    report_boundary_error("xlAutoClose module residency release", &error);
                    runtime.quarantine_runtime();
                }
            } else {
                // A safe Addin may own executable sources outside framework
                // accounting. Keep the DLL resident unless the Addin explicitly
                // accepted the physical-unload contract.
                runtime.clear_host_intent();
            }
        }
        1
    })
    .unwrap_or_else(|| {
        defer_opening_deactivation(runtime);
        1
    })
}

/// Handles the explicit terminal-removal boundary.
pub(crate) fn host_auto_remove<A>(runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>) -> i32
where
    A: Addin,
{
    with_host_lifecycle(ptr::from_ref(runtime).cast::<()>(), |_| {
        let mut callbacks = HostCallbackSession::new();
        deactivate_addin(runtime, &mut callbacks, "xlAutoRemove lifecycle thread");
        1
    })
    .unwrap_or_else(|| {
        defer_opening_deactivation(runtime);
        1
    })
}

fn deactivate_addin<A: Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
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
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
        type SharedState = ();
        type LifecycleState = CloseState;
        type Error = XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &OpenContext<Self>) -> Result<Opened<(), CloseState>, XllError> {
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
            .ensure_module_residency(
                host_auto_close::<ReentrantClose> as fn(&Runtime<ReentrantClose>) -> i32
                    as *const (),
            )
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
        with_host_lifecycle(ptr::from_ref(&outer).cast(), |_| {
            with_host_lifecycle(ptr::from_ref(&inner).cast(), |_| {
                assert_eq!(host_auto_close(&outer), 1);
                assert_eq!(host_auto_remove(&outer), 1);
                assert_eq!(outer.removal_epoch(), epoch);
            })
            .unwrap();
        })
        .unwrap();

        assert_eq!(host_auto_close(&outer), 1);
        assert!(outer.begin_open_if_epoch(epoch).is_err());
    }

    #[test]
    fn intrusive_close_boundary_is_thread_bound() {
        static_assertions::assert_not_impl_any!(HostCloseBoundary<'static>: Send, Sync);
    }

    #[test]
    fn miri_intrusive_deep_reentry_restores_lifo() {
        let (a, b, c) = (0_u8, 1_u8, 2_u8);
        let a = ptr::from_ref(&a).cast();
        let b = ptr::from_ref(&b).cast();
        let c = ptr::from_ref(&c).cast();
        assert_eq!(
            with_host_lifecycle(a, |_| {
                assert_eq!(
                    with_host_lifecycle(b, |_| {
                        assert_eq!(
                            with_host_lifecycle(c, |_| {
                                assert!(with_host_lifecycle(a, |_| ()).is_none());
                            }),
                            Some(())
                        );
                        assert!(with_host_lifecycle(b, |_| ()).is_none());
                    }),
                    Some(())
                );
                assert_eq!(with_host_lifecycle(b, |_| 2), Some(2));
                assert!(with_host_lifecycle(a, |_| ()).is_none());
            }),
            Some(())
        );
        assert_eq!(with_host_lifecycle(a, |_| 3), Some(3));
    }

    #[test]
    fn miri_intrusive_unwind_restores_real_tls_head() {
        let identity = 0_u8;
        let identity = ptr::from_ref(&identity).cast();
        let caught = std::panic::catch_unwind(|| {
            with_host_lifecycle(identity, |_| panic!("expected test panic"));
        });
        assert!(caught.is_err());
        assert_eq!(with_host_lifecycle(identity, |_| 4), Some(4));
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        clippy::forget_non_drop,
        reason = "Verify forgetting a borrowed capability cannot forget its private registration"
    )]
    fn miri_intrusive_forgotten_capability_cannot_keep_registration() {
        let runtime = 0_u8;
        let runtime = ptr::from_ref(&runtime).cast();
        assert_eq!(
            with_host_lifecycle(runtime, |frame| {
                std::mem::forget(HostCloseBoundary { frame });
            }),
            Some(())
        );
        assert_eq!(with_host_lifecycle(runtime, |_| 7), Some(7));
    }

    #[test]
    fn miri_intrusive_same_identity_is_independent_across_threads() {
        static ID: u8 = 0;
        let identity = ptr::from_ref(&ID).cast();
        with_host_lifecycle(identity, |_| {
            let other =
                std::thread::spawn(|| with_host_lifecycle(ptr::from_ref(&ID).cast(), |_| 9));
            assert_eq!(other.join().unwrap(), Some(9));
            assert!(with_host_lifecycle(identity, |_| ()).is_none());
        })
        .unwrap();
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
        use std::cell::RefCell;
        use xlfn_common::{ExecutionKind, FunctionVisibility};
        use xlfn_sys::{
            XL_EVENT_REGISTER, XL_FREE, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER,
            XLF_UNREGISTER, XLOPER12, XLOPER12Value, XLRET_FAILED, XLRET_SUCCESS, XLTYPE_STR,
        };

        struct ReloadAddin;

        impl Addin for ReloadAddin {
            #[cfg(feature = "rtd")]
            type RtdSubscription = ();
            #[cfg(feature = "rtd")]
            type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
            type SharedState = ();
            type LifecycleState = ();
            type Error = XllError;
            type Layers = ();
            #[cfg(feature = "async-builtin")]
            type AsyncExecutor = crate::BuiltinAsyncExecutor;
            #[cfg(all(feature = "async", not(feature = "async-builtin")))]
            type AsyncExecutor = crate::NoAsyncExecutor;

            fn open(_: &OpenContext<Self>) -> crate::addin::OpenResult<Self> {
                SCRIPT.with_borrow_mut(|script| script.opens += 1);
                let opened = Opened::new(());
                #[cfg(feature = "async-builtin")]
                let opened = opened.with_async_executor(crate::BuiltinAsyncExecutor::new(
                    crate::BuiltinExecutorConfig::new()
                        .with_poller_count(crate::AsyncPollerCount::new(32).unwrap()),
                ));
                Ok(opened)
            }

            fn quiesce(_: &mut (), _: &mut ()) -> Result<(), XllError> {
                SCRIPT.with_borrow_mut(|script| script.quiesces += 1);
                Ok(())
            }
        }

        // SAFETY: its state consists only of (). With async-builtin enabled,
        // every poller is owned and stopped by the framework executor protocol.
        // There are no application-owned execution sources.
        unsafe impl PhysicallyUnloadableAddin for ReloadAddin {}

        #[derive(Default)]
        struct ReloadScript {
            runtime: *const (),
            calls: Vec<i32>,
            opens: usize,
            quiesces: usize,
            registrations: usize,
            reenter_on_unregister: bool,
            reenter_on_open_callback: Option<i32>,
            reenter_with_remove: bool,
            reentry_status: i32,
            reentries: usize,
            reentry_preserved_owner: bool,
        }

        thread_local! {
            static SCRIPT: RefCell<ReloadScript> = RefCell::new(ReloadScript::default());
        }

        struct ReloadCallbacks<'runtime, P: crate::runtime::UnloadPolicy<ReloadAddin>> {
            _runtime: &'runtime Runtime<ReloadAddin, P>,
            _guard: crate::test_callback::CallbackTestGuard,
        }

        impl<'runtime, P: crate::runtime::UnloadPolicy<ReloadAddin>> ReloadCallbacks<'runtime, P> {
            fn install(runtime: &'runtime Runtime<ReloadAddin, P>) -> Self {
                let guard = crate::test_callback::lock();
                crate::module_runtime::reset_callbacks_for_test();
                SCRIPT.with_borrow_mut(|script| {
                    *script = ReloadScript {
                        runtime: std::ptr::from_ref(runtime).cast(),
                        ..ReloadScript::default()
                    };
                });
                // SAFETY: callback has Excel's exact ABI and is process-live;
                // the shared test guard excludes other callback fixtures.
                unsafe {
                    xlfn_sys::install_callback_for_abi_probe(
                        callback::<P> as *const () as *mut std::ffi::c_void,
                    );
                }
                Self {
                    _runtime: runtime,
                    _guard: guard,
                }
            }
        }

        impl<P: crate::runtime::UnloadPolicy<ReloadAddin>> Drop for ReloadCallbacks<'_, P> {
            fn drop(&mut self) {
                crate::test_callback::install();
                crate::module_runtime::reset_callbacks_for_test();
                SCRIPT.with_borrow_mut(|script| *script = ReloadScript::default());
            }
        }

        unsafe extern "system" fn callback<P: crate::runtime::UnloadPolicy<ReloadAddin>>(
            function: i32,
            _argument_count: i32,
            _arguments: *mut *mut XLOPER12,
            result: *mut XLOPER12,
        ) -> i32 {
            let reentry = SCRIPT.with_borrow_mut(|script| {
                script.calls.push(function);
                if script.reenter_on_open_callback == Some(function) {
                    script.reenter_on_open_callback = None;
                    Some(script.runtime)
                } else if function == XLF_UNREGISTER && script.reenter_on_unregister {
                    script.reenter_on_unregister = false;
                    Some(script.runtime)
                } else {
                    None
                }
            });
            if let Some(runtime) = reentry {
                // SAFETY: ReloadCallbacks retains the runtime borrow until
                // after this callback is uninstalled, on the same thread.
                let runtime = unsafe { &*runtime.cast::<Runtime<ReloadAddin, P>>() };
                let intent = runtime.host_intent();
                let epoch = runtime.removal_epoch();
                let remove = SCRIPT.with_borrow(|script| script.reenter_with_remove);
                let status = if remove {
                    host_auto_remove(runtime)
                } else {
                    host_auto_close(runtime)
                };
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
                XL_EVENT_REGISTER => XLOPER12::integer(1),
                _ => return XLRET_FAILED,
            };
            // SAFETY: Excel's callback contract supplies writable result
            // storage; any referenced test data remains process-live.
            unsafe { *result = value };
            if reentry.is_some() {
                SCRIPT.with_borrow(|script| script.reentry_status)
            } else {
                XLRET_SUCCESS
            }
        }

        fn open_for_reentry_test(
            runtime: &Runtime<ReloadAddin, impl crate::runtime::UnloadPolicy<ReloadAddin>>,
        ) -> i32 {
            host_auto_open(
                runtime,
                &AddinId::parse("opening-close").unwrap(),
                "0",
                "test",
                &[RegistrationDescriptor {
                    export_name: "test_opening_close_export",
                    excel_name: "TEST.OPENING.CLOSE",
                    signature: RegistrationSignature {
                        return_abi: xlfn_common::ReturnAbi::Value12,
                        execution: ExecutionKind::MainThread,
                        arguments: &[],
                        volatile: false,
                    },
                    category: "Test",
                    description: "reentrant close during opening",
                    help_topic: "",
                    visibility: FunctionVisibility::Public,
                    arguments: &[],
                }],
            )
        }

        #[test]
        fn opening_deactivation_is_not_lost_after_host_callback_returns() {
            for remove in [false, true] {
                for callback in [XL_GET_NAME, XL_EVENT_REGISTER, XLF_EVALUATE, XLF_REGISTER] {
                    let runtime = Runtime::<ReloadAddin>::new();
                    let _callbacks = ReloadCallbacks::install(&runtime);
                    runtime
                        .ensure_module_residency(
                            host_auto_close::<ReloadAddin>
                                as fn(&Runtime<ReloadAddin, crate::runtime::LogicalUnload>) -> i32
                                as *const (),
                        )
                        .unwrap();
                    SCRIPT.with_borrow_mut(|script| {
                        script.reenter_on_open_callback = Some(callback);
                        script.reenter_with_remove = remove;
                    });
                    let result = open_for_reentry_test(&runtime);
                    let phase = runtime.phase();
                    // Recover the old broken path before asserting, so a
                    // failure cannot strand the process-global test epoch.
                    if phase == LifecyclePhase::Open {
                        host_auto_close(&runtime);
                    }
                    assert_eq!(result, 0, "remove={remove}, callback={callback}");
                    assert_eq!(phase, LifecyclePhase::Closed);
                    #[cfg(feature = "async")]
                    assert!(runtime.async_runtime().is_stopped());
                    SCRIPT.with_borrow(|script| {
                        assert_eq!(script.reentries, 1);
                        assert_eq!(script.opens, script.quiesces);
                        assert_eq!(
                            script
                                .calls
                                .iter()
                                .filter(|&&call| call == XLF_UNREGISTER)
                                .count(),
                            script.registrations,
                        );
                    });
                    // The deferred request belongs only to the canceled open.
                    // A fresh host invocation may open and close normally.
                    assert_eq!(open_for_reentry_test(&runtime), 1);
                    assert_eq!(host_auto_close(&runtime), 1);
                    assert_eq!(runtime.phase(), LifecyclePhase::Closed);
                }
            }
        }

        #[test]
        fn opening_deactivation_does_not_renew_a_terminal_callback_session() {
            for status in [xlfn_sys::XLRET_ABORT, xlfn_sys::XLRET_UNCALCED] {
                let runtime = Runtime::<ReloadAddin>::new();
                let _callbacks = ReloadCallbacks::install(&runtime);
                // Quarantine deliberately retains the production router.
                // This fixture must release that global test state itself.
                let _diagnostics = scopeguard::guard((), |_| {
                    let _ = crate::diagnostics::close_diagnostic_router();
                });
                runtime
                    .ensure_module_residency(
                        host_auto_close::<ReloadAddin>
                            as fn(&Runtime<ReloadAddin, crate::runtime::LogicalUnload>) -> i32
                            as *const (),
                    )
                    .unwrap();
                SCRIPT.with_borrow_mut(|script| {
                    script.reenter_on_open_callback = Some(XLF_REGISTER);
                    script.reentry_status = status;
                });
                assert_eq!(open_for_reentry_test(&runtime), 0);
                assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
                #[cfg(feature = "async")]
                assert!(runtime.async_runtime().is_stopped());
                assert!(runtime.module_residency_held());
                SCRIPT.with_borrow(|script| {
                    assert_eq!(script.reentries, 1);
                    assert_eq!(
                        script.calls.iter().rfind(|&&call| call != XL_FREE),
                        Some(&XLF_REGISTER),
                    );
                });
                assert_eq!(host_auto_close(&runtime), 1);
                assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
            }
        }

        #[test]
        fn controlled_reload_coalesces_close_reentered_from_unregister() {
            let runtime = Runtime::<ReloadAddin>::new_with_physical_unload();
            let _callbacks = ReloadCallbacks::install(&runtime);
            runtime
                .ensure_module_residency(
                    host_auto_close::<ReloadAddin>
                        as fn(&Runtime<ReloadAddin, crate::runtime::PhysicalUnload>) -> i32
                        as *const (),
                )
                .unwrap();
            let descriptors = [RegistrationDescriptor {
                export_name: "test_reload_export",
                excel_name: "TEST.RELOAD.EXPORT",
                signature: RegistrationSignature {
                    return_abi: xlfn_common::ReturnAbi::Value12,
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
            assert_eq!(runtime.resident_event_count(), 1);
            assert!(runtime.module_residency_held());
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
