use super::automation::{
    server_get_ids_of_names, server_invoke, topic_key_from_safearray, write_bstr_variant,
    write_refresh_data, write_value_variant,
};
use super::com_abi::IID_IUNKNOWN;
use super::global_interface_table::get_git;
use super::module_lifetime;
use super::module_state::{ComObjectKind, ComObjectLease};
use super::registration::guid_compact;
use super::server_gate::{
    ServerCloseError, ServerOperationBarrier, ServerPhase, ServerTerminationRequest,
    TerminationWorker,
};
use super::update_event::{
    CallbackPtr, GitCookieLease, RetainedUpdateCallback, RtdNotifier, RtdUpdateEvent,
    ServerCallbacks, active_callback, drain_callbacks, install_callback, retry_git_revocation_debt,
};
use super::{com_boundary, guid_eq};
use crate::error::InputError;
use crate::handle::{FormulaLifetimeBackend, FormulaLifetimeConnection, FormulaLifetimeGeneration};
use crate::subscription::ServerGeneration;
use crate::subscription::SubscriptionRuntime;
use crate::win32::{
    CoCreateGuid, DISP_E_BADINDEX, DISPPARAMS, E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL,
    E_POINTER, EXCEPINFO, GUID, S_OK, SAFEARRAY, VARIANT, VARIANT_BOOL, VARIANT_FALSE,
    VARIANT_TRUE, VariantClear,
};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::ThreadId;

pub(super) const IID_IDISPATCH: GUID = GUID::from_u128(0x0002_0400_0000_0000_c000_0000_0000_0046);
pub(super) const IID_IRTD_SERVER: GUID = GUID::from_u128(0xec0e6191_db51_11d3_8f3e_00c04f3651b8);

pub(super) const IID_IRTD_UPDATE_EVENT: GUID =
    GUID::from_u128(0xa43788c1_d91b_11d3_8f39_00c04f3651b8);
pub(super) const SERVER_NOT_STARTED: u8 = 0;
pub(super) const SERVER_STARTING: u8 = 1;
pub(super) const SERVER_STARTED: u8 = 2;
pub(super) const SERVER_START_FAILED: u8 = 3;
#[derive(Clone)]
pub(super) struct ActiveServer {
    pub(super) class_id: GUID,
    pub(super) prog_id: String,
    pub(super) pointer: usize,
    pub(super) generation: ServerGeneration,
}

pub(super) static ACTIVE_SERVER: Mutex<Option<ActiveServer>> = Mutex::new(None);
pub(super) static LAST_SERVER_GENERATION: AtomicU64 = AtomicU64::new(0);

fn lifetime_generation(generation: ServerGeneration) -> FormulaLifetimeGeneration {
    FormulaLifetimeGeneration::new(generation.get())
        .expect("an active Excel RTD server has a non-zero lifetime generation")
}

fn allocate_server_generation(last_generation: &AtomicU64) -> Option<ServerGeneration> {
    // `try_update` returns the previous value; expose the checked successor
    // as the allocated generation and leave the counter unchanged at MAX.
    last_generation
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            last.checked_add(1)
        })
        .ok()
        .and_then(|last| last.checked_add(1))
        .and_then(ServerGeneration::new)
}

#[cfg(test)]
pub(super) static PANIC_IN_REFRESH_DATA: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(super) static FAIL_DEFERRED_TERMINATION_SPAWN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(super) static PANIC_DEFERRED_TERMINATION_CLEANUP: AtomicBool = AtomicBool::new(false);

pub(super) struct EnsuredServer {
    pub(super) active: ActiveServer,
    pub(super) newly_created: bool,
    pub(super) subscription_server: Option<crate::subscription::SubscriptionServerHandle>,
}

impl Drop for EnsuredServer {
    fn drop(&mut self) {
        // SAFETY: `ensure_server` acquires one temporary reference specifically
        // for this guard, so dropping the guard must release exactly that reference.
        unsafe { server_release(self.active.pointer as *mut RtdServer) };
    }
}

#[repr(C)]
pub(super) struct RtdServer {
    pub(super) vtable: *const RtdServerVtable,
    pub(super) references: AtomicU32,
    pub(super) start_state: AtomicU8,
    pub(super) generation: ServerGeneration,
    pub(super) operations: ServerOperationBarrier,
    pub(super) termination_worker: TerminationWorker,
    pub(super) backends: Mutex<ServerBackends>,
    pub(super) callbacks: Mutex<ServerCallbacks>,
    // Keep the module hold until every other field has been destroyed.
    pub(super) _module_lease: ComObjectLease,
}

pub(super) struct ServerStartReservation<'a> {
    pub(super) state: &'a AtomicU8,
    pub(super) committed: bool,
    pub(super) rollback_state: u8,
}

impl<'a> ServerStartReservation<'a> {
    pub(super) fn acquire(state: &'a AtomicU8) -> Option<Self> {
        state
            .compare_exchange(
                SERVER_NOT_STARTED,
                SERVER_STARTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| ServerStartReservation {
                state,
                committed: false,
                rollback_state: SERVER_NOT_STARTED,
            })
    }

    pub(super) fn callback_published(&mut self) {
        // Once a GIT cookie is server-owned, a later failure must be terminal
        // for this server instance. Allowing another ServerStart would retain
        // one more external COM reference on every failed retry.
        self.rollback_state = SERVER_START_FAILED;
    }

    pub(super) fn commit(mut self) {
        self.state.store(SERVER_STARTED, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for ServerStartReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state.store(self.rollback_state, Ordering::Release);
        }
    }
}

struct OwnedServerReference {
    pub(super) pointer: usize,
}

impl OwnedServerReference {
    unsafe fn acquire(pointer: *mut RtdServer) -> Self {
        // SAFETY: the caller guarantees `pointer` is a live server reference.
        unsafe { server_add_ref(pointer) };
        Self {
            pointer: pointer as usize,
        }
    }
}

impl Drop for OwnedServerReference {
    fn drop(&mut self) {
        // SAFETY: acquire created exactly one reference owned by this guard.
        unsafe { server_release(self.pointer as *mut RtdServer) };
    }
}

#[derive(Clone, Copy)]
pub(super) struct BackendHandles(NonNull<dyn FormulaLifetimeBackend>);

// SAFETY: FormulaLifetimeBackend is Send + Sync and remains valid while attached to the server.
unsafe impl Send for BackendHandles {}
// SAFETY: FormulaLifetimeBackend immutable operations are thread-safe.
unsafe impl Sync for BackendHandles {}

impl BackendHandles {
    pub(super) fn new(handles: &(dyn FormulaLifetimeBackend + 'static)) -> Self {
        let raw = std::ptr::from_ref(handles).cast_mut();
        // SAFETY: `raw` originates from a valid reference and is non-null.
        unsafe { Self(NonNull::new_unchecked(raw)) }
    }
}

impl std::ops::Deref for BackendHandles {
    type Target = dyn FormulaLifetimeBackend;

    fn deref(&self) -> &Self::Target {
        // SAFETY: Excel RTD server outlives or is bounded by the lifecycle coordinator
        // which withdraws/shuts down the server before dropping the backend.
        unsafe { self.0.as_ref() }
    }
}

#[derive(Clone, Copy)]
pub(super) struct BackendSubscriptions(NonNull<SubscriptionRuntime>);

// SAFETY: SubscriptionRuntime is Send + Sync and remains valid while attached to the server.
unsafe impl Send for BackendSubscriptions {}
// SAFETY: SubscriptionRuntime methods are thread-safe.
unsafe impl Sync for BackendSubscriptions {}

impl BackendSubscriptions {
    pub(super) fn new(subscriptions: &SubscriptionRuntime) -> Self {
        let raw = std::ptr::from_ref(subscriptions).cast_mut();
        // SAFETY: `raw` originates from a valid reference and is non-null.
        unsafe { Self(NonNull::new_unchecked(raw)) }
    }
}

impl std::ops::Deref for BackendSubscriptions {
    type Target = SubscriptionRuntime;

    fn deref(&self) -> &Self::Target {
        // SAFETY: Excel RTD server outlives or is bounded by the lifecycle coordinator
        // which withdraws/shuts down the server before dropping the runtime.
        unsafe { self.0.as_ref() }
    }
}

pub(super) struct ServerBackends {
    pub(super) handles: Option<BackendHandles>,
    pub(super) subscriptions: Option<BackendSubscriptions>,
    pub(super) subscription_server: Option<crate::subscription::SubscriptionServerHandle>,
}

pub(super) fn synchronize_callback_notification(
    server: &RtdServer,
    callback: CallbackPtr,
) -> XllResult<()> {
    let subscription_server = server.backends.lock().subscription_server;
    let Some(subscription_server) = subscription_server else {
        return Ok(());
    };

    let notifier = RtdNotifier::new(callback, NonNull::from(&server.operations));
    subscription_server.attach_update_notifier(notifier)?;
    Ok(())
}

impl Drop for RtdServer {
    fn drop(&mut self) {
        if !self.termination_worker.is_idle_or_joined() {
            // Dropping a JoinHandle would detach live code from the DLL that
            // owns it. Every removal path must join before releasing the final
            // ACTIVE_SERVER reference.
            std::process::abort();
        }
        let subscription_server = self.backends.lock().subscription_server;
        if let Some(subscription_server) = subscription_server {
            subscription_server.detach_update_notifier();
        }

        drain_callbacks(&self.callbacks);
    }
}

#[repr(C)]
pub(super) struct RtdServerVtable {
    pub(super) query_interface:
        unsafe extern "system" fn(*mut RtdServer, *const GUID, *mut *mut c_void) -> i32,
    pub(super) add_ref: unsafe extern "system" fn(*mut RtdServer) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut RtdServer) -> u32,
    pub(super) get_type_info_count: unsafe extern "system" fn(*mut RtdServer, *mut u32) -> i32,
    pub(super) get_type_info:
        unsafe extern "system" fn(*mut RtdServer, u32, u32, *mut *mut c_void) -> i32,
    pub(super) get_ids_of_names: unsafe extern "system" fn(
        *mut RtdServer,
        *const GUID,
        *const *const u16,
        u32,
        u32,
        *mut i32,
    ) -> i32,
    pub(super) invoke: unsafe extern "system" fn(
        *mut RtdServer,
        i32,
        *const GUID,
        u32,
        u16,
        *mut DISPPARAMS,
        *mut VARIANT,
        *mut EXCEPINFO,
        *mut u32,
    ) -> i32,
    pub(super) server_start:
        unsafe extern "system" fn(*mut RtdServer, *mut c_void, *mut i32) -> i32,
    pub(super) connect_data: unsafe extern "system" fn(
        *mut RtdServer,
        i32,
        *mut *mut SAFEARRAY,
        *mut VARIANT_BOOL,
        *mut VARIANT,
    ) -> i32,
    pub(super) refresh_data:
        unsafe extern "system" fn(*mut RtdServer, *mut i32, *mut *mut SAFEARRAY) -> i32,
    pub(super) disconnect_data: unsafe extern "system" fn(*mut RtdServer, i32) -> i32,
    pub(super) heartbeat: unsafe extern "system" fn(*mut RtdServer, *mut i32) -> i32,
    pub(super) server_terminate: unsafe extern "system" fn(*mut RtdServer) -> i32,
}

pub(super) static RTD_SERVER_VTABLE: RtdServerVtable = RtdServerVtable {
    query_interface: server_query_interface,
    add_ref: server_add_ref,
    release: server_release,
    get_type_info_count: server_get_type_info_count,
    get_type_info: server_get_type_info,
    get_ids_of_names: server_get_ids_of_names,
    invoke: server_invoke,
    server_start,
    connect_data,
    refresh_data,
    disconnect_data,
    heartbeat,
    server_terminate,
};
pub(super) fn discard_unpublished_server(pointer: usize, newly_created: bool) {
    if !newly_created {
        return;
    }

    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| entry.pointer == pointer)
            .map(|_| {
                let server = pointer as *mut RtdServer;
                // SAFETY: ACTIVE_SERVER owns a live reference while locked.
                unsafe { server_add_ref(server) };
                server
            })
    };
    let Some(server) = retained else {
        return;
    };

    // This path normally runs before Excel can activate the new class. Still
    // use the full terminal/join protocol so an unexpected concurrent
    // ServerTerminate cannot detach a coordinator from the module.
    // SAFETY: the temporary reference above keeps the server live.
    let termination = match unsafe { (*server).operations.close_and_wait() } {
        Ok(termination) => termination,
        Err(_) => {
            // SAFETY: balance the temporary retained reference.
            unsafe { server_release(server) };
            return;
        }
    };
    // SAFETY: the temporary reference keeps the worker state live.
    let join_result = unsafe { (*server).termination_worker.join() };
    if matches!(join_result, Err(ServerCloseError::WorkerPanicked)) {
        crate::diagnostics::report_no_unwind(
            "IRtdServer::deferred termination join",
            &XllError::Panic,
        );
    } else if join_result.is_err() {
        std::process::abort();
    }

    // SAFETY: the server is quiescent and its worker has exited.
    let _ = unsafe { teardown_server_resources(server, true) };
    drop(termination);
    // SAFETY: balance the temporary retained reference.
    unsafe { server_release(server) };
}

#[cfg(any(feature = "handles", test))]
pub(crate) fn shutdown<H: FormulaLifetimeBackend + 'static>(handles: &H) -> XllResult<()> {
    let mut shutdown_error = None;
    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| {
                let server = entry.pointer as *mut RtdServer;

                // SAFETY: ACTIVE_SERVER owns a live reference to this RtdServer
                // while its mutex remains held.
                unsafe {
                    (*server)
                        .backends
                        .lock()
                        .handles
                        .as_ref()
                        .is_some_and(|active| active.identity() == handles.identity())
                }
            })
            .cloned()
            .inspect(|entry| {
                // SAFETY: ACTIVE_SERVER owns a live reference while its mutex
                // remains held. This temporary reference keeps the raw pointer
                // alive while shutdown waits outside the global mutex.
                unsafe { server_add_ref(entry.pointer as *mut RtdServer) };
            })
    };

    if let Some(retained) = retained {
        let server = retained.pointer as *mut RtdServer;

        // SAFETY: the temporary reference acquired above keeps `server` alive.
        let _server_termination = match unsafe { (*server).operations.close_and_wait() } {
            Ok(termination) => termination,
            Err(_) => {
                // Excel lifecycle close is serialized on its main thread.
                // A same-thread entered COM operation or recursively owned
                // termination violates that contract; returning would let
                // the host unload live code.
                std::process::abort();
            }
        };
        // A deferred ServerTerminate signals phase completion before its Rust
        // thread exits. Join before executing any code that may release the
        // module-lifetime ACTIVE_SERVER reference.
        // SAFETY: the temporary reference keeps `server` live.
        match unsafe { (*server).termination_worker.join() } {
            Ok(()) => {}
            Err(ServerCloseError::WorkerPanicked) => {
                let error = XllError::Panic;
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination join",
                    &error,
                );
                shutdown_error = Some(error);
            }
            Err(_) => std::process::abort(),
        }
        handles.terminate_topics(lifetime_generation(retained.generation));
        // No operation can retain or invoke the callback after the server gate
        // and handle topics are quiescent. Revoke the GIT cookie while the XLL
        // is still loaded, without holding the callback or global server lock.
        // SAFETY: the temporary reference acquired above keeps `server` alive.
        unsafe { drain_callbacks(&(*server).callbacks) };

        let removed = {
            let mut active = ACTIVE_SERVER.lock();
            active
                .as_ref()
                .is_some_and(|entry| entry.pointer == retained.pointer)
                .then(|| active.take())
                .flatten()
                .is_some()
        };

        if removed {
            // SAFETY: removing the ACTIVE_SERVER entry transferred its owned
            // reference to this branch, which releases it exactly once.
            unsafe { server_release(server) };
        }

        // Finish the phase transition while the temporary server reference
        // still guarantees that the borrowed barrier remains alive.
        drop(_server_termination);

        // SAFETY: balance the temporary reference acquired while locating the
        // active server.
        unsafe { server_release(server) };
    } else {
        handles.terminate_all_topics();
    }
    retry_git_revocation_debt();
    match shutdown_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn shutdown_subscriptions(subscriptions: &SubscriptionRuntime) -> XllResult<()> {
    let mut shutdown_error = None;
    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| {
                let server = entry.pointer as *mut RtdServer;

                // SAFETY: ACTIVE_SERVER owns a live reference to this RtdServer
                // while its mutex remains held.
                unsafe {
                    (*server)
                        .backends
                        .lock()
                        .subscriptions
                        .as_ref()
                        .is_some_and(|active| std::ptr::eq(&**active, subscriptions))
                }
            })
            .cloned()
            .inspect(|entry| {
                // SAFETY: ACTIVE_SERVER owns a live reference while its mutex
                // remains held. This temporary reference keeps the raw pointer
                // alive while shutdown waits outside the global mutex.
                unsafe { server_add_ref(entry.pointer as *mut RtdServer) };
            })
    };

    if let Some(retained) = retained {
        let server = retained.pointer as *mut RtdServer;

        // Stop all COM methods first. SubscriptionRuntime::close then closes
        // background publish/notification work that does not enter through COM.
        // SAFETY: the temporary reference acquired above keeps `server` alive.
        let _server_termination = match unsafe { (*server).operations.close_and_wait() } {
            Ok(termination) => termination,
            Err(_) => {
                // See the corresponding handle-shutdown branch above:
                // terminal unload cannot continue through a recursively
                // owned server operation.
                std::process::abort();
            }
        };
        // SAFETY: the temporary reference keeps `server` live.
        match unsafe { (*server).termination_worker.join() } {
            Ok(()) => {}
            Err(ServerCloseError::WorkerPanicked) => {
                let error = XllError::Panic;
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination join",
                    &error,
                );
                shutdown_error = Some(error);
            }
            Err(_) => std::process::abort(),
        }
        let close_result = subscriptions.close();
        // `subscriptions.close` revoked notification closures and waited for
        // cloned callbacks. Revoke the server's final GIT reference before
        // returning from XLL shutdown, outside all server/runtime locks.
        // SAFETY: the temporary reference acquired above keeps `server` alive.
        unsafe { drain_callbacks(&(*server).callbacks) };

        let removed = {
            let mut active = ACTIVE_SERVER.lock();
            active
                .as_ref()
                .is_some_and(|entry| entry.pointer == retained.pointer)
                .then(|| active.take())
                .flatten()
                .is_some()
        };

        if removed {
            // SAFETY: removing the ACTIVE_SERVER entry transferred its owned
            // reference to this branch, which releases it exactly once.
            unsafe { server_release(server) };
        }

        // Finish the phase transition while the temporary server reference
        // still guarantees that the borrowed barrier remains alive.
        drop(_server_termination);

        // SAFETY: balance the temporary reference acquired while locating the
        // active server.
        unsafe { server_release(server) };
        retry_git_revocation_debt();
        match shutdown_error {
            Some(error) => Err(error),
            None => close_result,
        }
    } else {
        let close_result = subscriptions.close();
        retry_git_revocation_debt();
        close_result
    }
}

#[cfg(any(feature = "handles", test))]
pub(super) fn ensure_server<H: FormulaLifetimeBackend + 'static>(
    handles: Option<&H>,
    subscriptions: Option<&SubscriptionRuntime>,
) -> XllResult<EnsuredServer> {
    let backend = handles.map(|h| -> &(dyn FormulaLifetimeBackend + 'static) { h });
    ensure_server_impl(backend, subscriptions)
}

pub(super) fn ensure_server_without_handles(
    subscriptions: Option<&SubscriptionRuntime>,
) -> XllResult<EnsuredServer> {
    ensure_server_impl(None, subscriptions)
}

fn ensure_server_impl(
    handles: Option<&(dyn FormulaLifetimeBackend + 'static)>,
    subscriptions: Option<&SubscriptionRuntime>,
) -> XllResult<EnsuredServer> {
    let mut active = ACTIVE_SERVER.lock();

    if let Some(existing) = active.as_ref() {
        let server = existing.pointer as *mut RtdServer;
        // Deferred ServerTerminate intentionally leaves ACTIVE_SERVER in place
        // until a host lifecycle path can join its coordinator. Reap a fully
        // terminated entry before constructing or attaching a replacement.
        // SAFETY: ACTIVE_SERVER owns a live server reference while locked.
        let operations = unsafe { &(*server).operations };
        let phase = operations.state.lock().phase;
        if phase == ServerPhase::Terminated {
            // SAFETY: create a temporary reference that remains valid after the
            // global mutex is released for the potentially blocking join.
            unsafe { server_add_ref(server) };
            drop(active);

            // SAFETY: the temporary reference above keeps `server` live.
            let join_result = unsafe { (*server).termination_worker.join() };
            if matches!(join_result, Err(ServerCloseError::Reentrant)) {
                // SAFETY: balance the temporary reference above. A coordinator
                // re-entering ensure_server must let an external thread reap it.
                unsafe { server_release(server) };
                return Err(XllError::Closing);
            }
            if matches!(join_result, Err(ServerCloseError::WorkerPanicked)) {
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination join",
                    &XllError::Panic,
                );
            }

            // Idempotently finish any cleanup skipped by a panicking worker and
            // transfer/release the ACTIVE_SERVER reference.
            // SAFETY: the temporary reference keeps `server` live.
            let _ = unsafe { teardown_server_resources(server, true) };
            // SAFETY: balance the temporary reference acquired above.
            unsafe { server_release(server) };
            return ensure_server_impl(handles, subscriptions);
        }

        // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
        // held. A closing server cannot accept a newly attached backend.
        let _server_operation = unsafe { (*server).operations.enter() }.ok_or(XllError::Closing)?;

        // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
        // held, so the RtdServer and its `backends` mutex are valid.
        let mut backends = unsafe { (*server).backends.lock() };

        if let Some(handles) = handles {
            match backends.handles.as_ref() {
                Some(active) if active.identity() == handles.identity() => {}
                Some(_) => {
                    return Err(XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
                    });
                }
                None => {
                    backends.handles = Some(BackendHandles::new(handles));
                }
            }
        }

        let (newly_attached_subscriptions, subscription_handle) =
            if let Some(subscriptions) = subscriptions {
                match backends.subscriptions.as_ref() {
                    Some(active) if std::ptr::eq(&**active, subscriptions) => {
                        (None, backends.subscription_server)
                    }
                    Some(_) => {
                        return Err(XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
                        });
                    }
                    None => {
                        let handle = subscriptions.register_server(existing.generation)?;
                        backends.subscriptions = Some(BackendSubscriptions::new(subscriptions));
                        backends.subscription_server = Some(handle);
                        (Some(handle), Some(handle))
                    }
                }
            } else {
                (None, backends.subscription_server)
            };

        drop(backends);

        if let Some(handle) = newly_attached_subscriptions {
            // SAFETY: `server` was validated as non-null and COM keeps the server alive.
            let callback = unsafe { active_callback(&(*server).callbacks) };
            if let Some(callback) = callback {
                let notifier = RtdNotifier::new(callback, NonNull::from(operations));
                handle.attach_update_notifier(notifier)?;
            }
        }

        // SAFETY: ACTIVE_SERVER owns a live server reference. This increments
        // the count to create the separate temporary reference held by the guard.
        unsafe { server_add_ref(server) };

        return Ok(EnsuredServer {
            active: existing.clone(),
            newly_created: false,
            subscription_server: subscription_handle,
        });
    }

    let mut class_id = GUID::from_u128(0);

    // SAFETY: `class_id` points to writable GUID storage.
    let status = unsafe { CoCreateGuid(&mut class_id) };

    if status < 0 {
        return Err(XllError::WindowsApi {
            function: "CoCreateGuid",
            code: status,
        });
    }

    let generation =
        allocate_server_generation(&LAST_SERVER_GENERATION).ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SERVER_GENERATION_EXHAUSTED,
        })?;
    let operations = ServerOperationBarrier::new().map_err(|error| XllError::WindowsApi {
        function: error.operation,
        code: error.code as i32,
    })?;

    let subscription_handle = if let Some(subscriptions) = subscriptions {
        Some(subscriptions.register_server(generation)?)
    } else {
        None
    };

    let server = Box::new(RtdServer {
        vtable: &RTD_SERVER_VTABLE,
        references: AtomicU32::new(1),
        start_state: AtomicU8::new(SERVER_NOT_STARTED),
        generation,
        operations,
        termination_worker: TerminationWorker::default(),
        backends: Mutex::new(ServerBackends {
            handles: handles.map(BackendHandles::new),
            subscriptions: subscriptions.map(BackendSubscriptions::new),
            subscription_server: subscription_handle,
        }),
        callbacks: Mutex::new(ServerCallbacks::default()),
        _module_lease: ComObjectLease::new(ComObjectKind::Server),
    });

    let pointer = Box::into_raw(server) as usize;
    let entry = ActiveServer {
        class_id,
        prog_id: format!("XlFnRtd_{}", guid_compact(class_id)),
        pointer,
        generation,
    };

    *active = Some(entry.clone());

    // SAFETY: the construction reference is now owned by ACTIVE_SERVER.
    // Incrementing here creates a separate temporary reference for EnsuredServer.
    unsafe { server_add_ref(pointer as *mut RtdServer) };

    Ok(EnsuredServer {
        active: entry,
        newly_created: true,
        subscription_server: subscription_handle,
    })
}
pub(super) unsafe extern "system" fn server_query_interface(
    this: *mut RtdServer,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    let _module_call = module_lifetime().enter_call();
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to writable storage.
    unsafe { *output = ptr::null_mut() };

    if this.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: `interface_id` was validated as non-null and COM supplies a
    // readable GUID for the duration of this method.
    let interface_id = unsafe { *interface_id };

    if guid_eq(interface_id, IID_IUNKNOWN)
        || guid_eq(interface_id, IID_IDISPATCH)
        || guid_eq(interface_id, IID_IRTD_SERVER)
    {
        // SAFETY: `output` is writable and `this` is a live server pointer.
        // AddRef creates the reference returned through `output`.
        unsafe {
            *output = this.cast();
            server_add_ref(this);
        }

        S_OK
    } else {
        E_NOINTERFACE
    }
}

pub(super) unsafe extern "system" fn server_add_ref(this: *mut RtdServer) -> u32 {
    let _module_call = module_lifetime().enter_call();
    // SAFETY: COM and internal callers invoke AddRef only on a live RtdServer.
    unsafe { (*this).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

pub(super) unsafe extern "system" fn server_release(this: *mut RtdServer) -> u32 {
    let _module_call = module_lifetime().enter_call();
    let Some(this) = NonNull::new(this) else {
        return 0;
    };

    // SAFETY: callers release only an outstanding reference to a live server.
    // A zero previous count is an invariant violation and must never wrap to
    // `u32::MAX`.
    let previous = unsafe { this.as_ref().references.fetch_sub(1, Ordering::AcqRel) };
    if previous == 0 {
        std::process::abort();
    }
    let remaining = previous - 1;

    if remaining == 0 {
        // SAFETY: the transition to zero proves exclusive ownership of the Box
        // allocation originally produced by Box::into_raw.
        drop(unsafe { Box::from_raw(this.as_ptr()) });
    }

    remaining
}

unsafe extern "system" fn server_get_type_info_count(this: *mut RtdServer, count: *mut u32) -> i32 {
    let _module_call = module_lifetime().enter_call();
    if this.is_null() || count.is_null() {
        return E_POINTER;
    }

    // SAFETY: `count` was validated as non-null and is a writable COM output.
    unsafe { *count = 0 };

    S_OK
}

unsafe extern "system" fn server_get_type_info(
    this: *mut RtdServer,
    index: u32,
    _locale: u32,
    output: *mut *mut c_void,
) -> i32 {
    let _module_call = module_lifetime().enter_call();
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and COM supplied it as a
    // writable output slot.
    unsafe { *output = ptr::null_mut() };

    if this.is_null() {
        E_POINTER
    } else if index != 0 {
        DISP_E_BADINDEX
    } else {
        // GetTypeInfoCount reports zero. The RTD dispatch surface remains
        // programmable through GetIDsOfNames/Invoke without a runtime type
        // information object.
        E_NOTIMPL
    }
}

pub(super) unsafe extern "system" fn server_start(
    this: *mut RtdServer,
    callback: *mut c_void,
    result: *mut i32,
) -> i32 {
    com_boundary("IRtdServer::ServerStart", || {
        // SAFETY: the arguments are forwarded from the COM vtable call. The
        // inner function validates each pointer before dereferencing it.
        unsafe { server_start_inner(this, callback, result) }
    })
}

unsafe fn server_start_inner(this: *mut RtdServer, callback: *mut c_void, result: *mut i32) -> i32 {
    if this.is_null() || callback.is_null() || result.is_null() {
        return E_POINTER;
    }

    // SAFETY: `this` was validated as non-null and COM owns a live reference
    // throughout this method.
    let _server_operation = match unsafe { (*this).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    // SAFETY: `result` was validated as non-null and points to writable COM
    // output storage.
    unsafe { *result = 0 };

    // Excel's RTD contract starts one freshly created server exactly once.
    // Reserve that transition atomically so automation clients cannot grow an
    // unbounded set of retained GIT cookies through repeated ServerStart calls.
    // SAFETY: `this` remains live for the complete COM method.
    let start_state = unsafe { &(*this).start_state };
    let Some(mut start_reservation) = ServerStartReservation::acquire(start_state) else {
        return E_FAIL;
    };

    // SAFETY: `this` was validated as non-null and COM keeps the server alive
    // for the duration of ServerStart.
    let _subscription_server = unsafe { (*this).backends.lock().subscription_server };

    let callback_ptr = callback.cast::<RtdUpdateEvent>();
    let mut cookie = 0u32;

    // A previous termination may have been unable to enter COM or revoke its
    // GIT cookie. Retry before adding another process-wide callback reference.
    retry_git_revocation_debt();

    // SAFETY: the caller entered this method through COM, so the current thread
    // has a usable COM apartment. `get_git` returns one owned GIT wrapper or an
    // HRESULT error.
    let git = unsafe { get_git() };

    let Ok(git) = git else {
        return E_FAIL;
    };

    // SAFETY: `git` owns a live IGlobalInterfaceTable, `callback_ptr` is the
    // live IRTDUpdateEvent supplied by Excel, the IID is valid, and `cookie`
    // is writable.
    let status = unsafe { git.register(callback_ptr.cast(), &IID_IRTD_UPDATE_EVENT, &mut cookie) };

    if status < 0 {
        return E_FAIL;
    }

    let Some(cookie) = NonZeroU32::new(cookie) else {
        return E_FAIL;
    };

    // Track the successfully registered GIT cookie before any later fallible
    // callback publication or notification work can run.
    let cookie = GitCookieLease::from_registered(cookie);

    let callback = Box::new(RetainedUpdateCallback {
        cookie: Some(cookie),
        #[cfg(test)]
        drop_hook: None,
    });

    // SAFETY: `this` was validated as non-null and COM keeps the server alive
    // for the duration of ServerStart.
    let callback_ptr = unsafe { install_callback(&(*this).callbacks, callback) };
    start_reservation.callback_published();

    // SAFETY: `this` remains live through the COM call. Re-reading backends
    // after installing the callback closes the race with a concurrently
    // attached subscription runtime.
    if unsafe { synchronize_callback_notification(&*this, callback_ptr) }.is_err() {
        return E_FAIL;
    }

    start_reservation.commit();

    // SAFETY: `result` was validated as non-null and remains valid for the
    // duration of this COM method.
    unsafe { *result = 1 };

    S_OK
}

pub(super) unsafe extern "system" fn connect_data(
    this: *mut RtdServer,
    topic_id: i32,
    strings: *mut *mut SAFEARRAY,
    new_values: *mut VARIANT_BOOL,
    result: *mut VARIANT,
) -> i32 {
    com_boundary("IRtdServer::ConnectData", || {
        // SAFETY: arguments are forwarded from the COM vtable invocation. The
        // inner function validates all pointers before dereferencing them.
        unsafe { connect_data_inner(this, topic_id, strings, new_values, result) }
    })
}

enum ConnectDataTransaction<'runtime> {
    Handle(Box<dyn FormulaLifetimeConnection + 'runtime>),
    Subscription(crate::subscription::SubscriptionConnection),
}

impl ConnectDataTransaction<'_> {
    unsafe fn write_value(&self, result: *mut VARIANT) -> i32 {
        match self {
            // SAFETY: caller validated result as non-null and writable; token remains readable.
            Self::Handle(connection) => unsafe { write_bstr_variant(result, connection.token()) },
            // SAFETY: caller validated result as non-null and writable; value remains readable.
            Self::Subscription(connection) => unsafe {
                write_value_variant(result, connection.value())
            },
        }
    }

    fn commit(self) -> XllResult<()> {
        match self {
            Self::Handle(connection) => connection.commit(),
            Self::Subscription(connection) => connection.commit(),
        }
    }
}

unsafe fn connect_data_inner(
    this: *mut RtdServer,
    topic_id: i32,
    strings: *mut *mut SAFEARRAY,
    new_values: *mut VARIANT_BOOL,
    result: *mut VARIANT,
) -> i32 {
    if this.is_null() || strings.is_null() || new_values.is_null() || result.is_null() {
        return E_POINTER;
    }

    // SAFETY: both output pointers were validated as non-null and COM supplies
    // writable storage for the duration of ConnectData. Initialize every
    // failure path before entering runtime or user code.
    unsafe {
        ptr::write(result, VARIANT::default());
        *new_values = VARIANT_FALSE;
    }

    // SAFETY: `this` was validated as non-null and COM owns a live reference
    // throughout this method.
    let _server_operation = match unsafe { (*this).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    // The SAFEARRAY is the authoritative per-call topic identity. Keeping a
    // shared pending side channel here would make nested Excel callbacks able
    // to overwrite one another.

    // SAFETY: `strings` was validated as non-null and Excel supplies the live
    // RTD topic SAFEARRAY for the duration of this call.
    let Ok(key) = (unsafe { topic_key_from_safearray(strings) }) else {
        let error = XllError::input("RTD topic", InputError::Malformed("invalid topic array"));
        crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
        return E_INVALIDARG;
    };

    let (handles, subscriptions, subscription_server) = {
        // SAFETY: `this` was validated as non-null and COM retains the server during ConnectData.
        let backends = unsafe { (*this).backends.lock() };
        (
            backends.handles,
            backends.subscriptions,
            backends.subscription_server,
        )
    };

    // SAFETY: `this` remains valid for the duration of ConnectData.
    let generation = unsafe { (*this).generation };

    let connection = if let Some(rtd_key) = key.strip_prefix("handle:") {
        let Some(handles) = handles.as_ref() else {
            return E_FAIL;
        };

        match handles.connect_lifetime(lifetime_generation(generation), topic_id, rtd_key) {
            Ok(connection) => ConnectDataTransaction::Handle(connection),
            Err(error) => {
                crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
                return E_FAIL;
            }
        }
    } else if let Ok(sub_key) = crate::subscription::SubscriptionKey::parse_transport(&key) {
        let Some(subscription_server) = subscription_server.as_ref() else {
            return E_FAIL;
        };
        let Some(subscriptions) = subscriptions.as_ref() else {
            return E_FAIL;
        };

        let sub_id = match subscriptions.resolve_transport_key(sub_key) {
            Ok(id) => id,
            Err(error) => {
                crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
                return E_FAIL;
            }
        };

        match subscription_server
            .connect_transaction(crate::subscription::TopicId(topic_id), sub_id)
        {
            Ok(connection) => ConnectDataTransaction::Subscription(connection),
            Err(error) => {
                crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
                return E_FAIL;
            }
        }
    } else {
        return E_INVALIDARG;
    };

    // SAFETY: `result` was validated as non-null and points to writable VARIANT
    // storage supplied by COM.
    let status = unsafe { connection.write_value(result) };

    if status != S_OK {
        // Dropping the uncommitted transaction rolls back only the connection
        // created by this call. Existing shared connections remain untouched.
        drop(connection);
        return status;
    }

    if let Err(error) = connection.commit() {
        crate::diagnostics::report_no_unwind("IRtdServer::ConnectData commit", &error);
        // SAFETY: write_value_variant succeeded, so result contains a valid
        // initialized VARIANT whose owned resources must be released before
        // returning failure to COM.
        let _ = unsafe { VariantClear(result) };
        return E_FAIL;
    }

    // SAFETY: `new_values` was validated as non-null and remains writable for
    // the duration of this method.
    unsafe { *new_values = VARIANT_TRUE };
    S_OK
}

pub(super) unsafe extern "system" fn refresh_data(
    this: *mut RtdServer,
    topic_count: *mut i32,
    result: *mut *mut SAFEARRAY,
) -> i32 {
    com_boundary("IRtdServer::RefreshData", || {
        // SAFETY: arguments are forwarded from the COM vtable invocation. The
        // inner function validates all pointers before dereferencing them.
        unsafe { refresh_data_inner(this, topic_count, result) }
    })
}

unsafe fn refresh_data_inner(
    this: *mut RtdServer,
    topic_count: *mut i32,
    result: *mut *mut SAFEARRAY,
) -> i32 {
    #[cfg(test)]
    if PANIC_IN_REFRESH_DATA.swap(false, Ordering::AcqRel) {
        panic!("injected RefreshData panic");
    }

    if this.is_null() || topic_count.is_null() || result.is_null() {
        return E_POINTER;
    }

    // SAFETY: `this` was validated as non-null and COM owns a live reference
    // throughout this method.
    let _server_operation = match unsafe { (*this).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    // SAFETY: `this` was validated as non-null and COM retains the server for
    // the duration of RefreshData.
    let subscription_server = unsafe { (*this).backends.lock().subscription_server };

    let Some(subscription_server) = subscription_server else {
        // SAFETY: `topic_count` and `result` are valid COM output parameters.
        return unsafe { write_refresh_data(topic_count, result, &[]) };
    };

    let batch = match subscription_server.begin_refresh() {
        Ok(batch) => batch,
        Err(error) => {
            crate::diagnostics::report_no_unwind("IRtdServer::RefreshData begin", &error);
            return E_FAIL;
        }
    };

    // SAFETY: `topic_count` and `result` are valid COM output parameters.
    let status = unsafe { write_refresh_data(topic_count, result, &batch.updates) };

    let outcome = if status == S_OK {
        crate::subscription::RefreshOutcome::Delivered
    } else {
        crate::subscription::RefreshOutcome::Failed
    };

    if let Err(error) = batch.complete(outcome) {
        crate::diagnostics::report_no_unwind("IRtdServer::RefreshData rearm", &error);
    }

    status
}

pub(super) unsafe extern "system" fn disconnect_data(this: *mut RtdServer, topic_id: i32) -> i32 {
    com_boundary("IRtdServer::DisconnectData", || {
        // SAFETY: arguments are forwarded from the COM vtable invocation. The
        // inner function validates `this` before dereferencing it.
        unsafe { disconnect_data_inner(this, topic_id) }
    })
}

unsafe fn disconnect_data_inner(this: *mut RtdServer, topic_id: i32) -> i32 {
    if this.is_null() {
        return E_POINTER;
    }

    // SAFETY: `this` was validated as non-null and COM owns a live reference
    // throughout this method.
    let _server_operation = match unsafe { (*this).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    let (handles, subscription_server) = {
        // SAFETY: `this` was validated as non-null and COM retains the server during DisconnectData.
        let backends = unsafe { (*this).backends.lock() };
        (backends.handles, backends.subscription_server)
    };

    // SAFETY: `this` remains valid for the duration of DisconnectData.
    let generation = unsafe { (*this).generation };

    if let Some(handles) = handles {
        handles.disconnect(lifetime_generation(generation), topic_id);
    }

    if let Some(subscription_server) = subscription_server.as_ref() {
        match subscription_server.disconnect(crate::subscription::TopicId(topic_id)) {
            Err(error) if !matches!(error, crate::XllError::Closing) => {
                crate::diagnostics::report_no_unwind("IRtdServer::DisconnectData", &error);
            }
            _ => {}
        }
    }

    S_OK
}

pub(super) unsafe extern "system" fn heartbeat(this: *mut RtdServer, result: *mut i32) -> i32 {
    com_boundary("IRtdServer::Heartbeat", || {
        if result.is_null() {
            return E_POINTER;
        }

        let _server_operation = if this.is_null() {
            None
        } else {
            // SAFETY: `this` was checked as non-null.
            match unsafe { (*this).operations.enter() } {
                Some(operation) => Some(operation),
                None => return E_FAIL,
            }
        };

        if !this.is_null() {
            // SAFETY: `this` was checked as non-null.
            let subscription_server = unsafe { (*this).backends.lock().subscription_server };
            if let Some(subscription_server) = subscription_server {
                let _ = subscription_server.pulse_notification();
            }
        }

        // SAFETY: `result` was validated as non-null and points to writable COM
        // output storage.
        unsafe { *result = 1 };

        S_OK
    })
}

pub(super) unsafe extern "system" fn server_terminate(this: *mut RtdServer) -> i32 {
    com_boundary("IRtdServer::ServerTerminate", || {
        // SAFETY: `this` is forwarded from the COM vtable invocation. The inner
        // function validates it before dereferencing.
        unsafe { server_terminate_inner(this) }
    })
}

unsafe fn server_terminate_inner(this: *mut RtdServer) -> i32 {
    if this.is_null() {
        return E_POINTER;
    }

    // Never synchronously wait from ServerTerminate: Excel can call it while
    // servicing an UpdateNotify marshalled from a publisher MTA. Linearize the
    // phase now and let a retained coordinator finish after the current call
    // unwinds when the server is not already quiescent.
    // SAFETY: `this` is live for the complete COM method invocation.
    let request = match unsafe { (*this).operations.request_termination() } {
        Ok(request) => request,
        Err(_) => return E_FAIL,
    };

    match request {
        ServerTerminationRequest::Complete | ServerTerminationRequest::InProgress => S_OK,
        ServerTerminationRequest::Synchronous(termination) => {
            // SAFETY: COM retains `this`; the synchronous termination guard
            // excludes every new server operation through the full teardown.
            let status = unsafe { teardown_server_resources(this, true) };
            drop(termination);
            status
        }
        ServerTerminationRequest::Deferred(reservation) => {
            // The reservation retains the barrier mutex across worker startup,
            // so a duplicate ServerTerminate cannot observe an in-progress
            // phase that will subsequently roll back after spawn failure.
            // SAFETY: `this` remains live through this COM call.
            let worker_start = match unsafe { (*this).termination_worker.reserve_start() } {
                Ok(start) => start,
                Err(_) => return E_FAIL,
            };
            // SAFETY: COM owns a live reference. This additional reference is
            // transferred into the coordinator closure (or dropped on failure).
            let reference = unsafe { OwnedServerReference::acquire(this) };
            let owner = reservation.owner;
            match spawn_deferred_termination(reference, owner) {
                Ok(handle) => {
                    worker_start.commit(handle);
                    reservation.commit();
                    S_OK
                }
                Err(_) => E_FAIL,
            }
        }
    }
}

fn spawn_deferred_termination(
    reference: OwnedServerReference,
    owner: ThreadId,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if FAIL_DEFERRED_TERMINATION_SPAWN.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other(
            "injected deferred RTD termination spawn failure",
        ));
    }

    std::thread::Builder::new()
        .name("xlfn-rtd-termination".to_owned())
        .spawn(move || deferred_termination_worker(reference, owner))
}

fn deferred_termination_worker(reference: OwnedServerReference, owner: ThreadId) {
    let this = reference.pointer as *mut RtdServer;
    // Keep the internal reference outside the unwind boundary. It remains live
    // until all cleanup and phase signaling have completed.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `reference` owns one live server reference.
        let termination = match unsafe { (*this).operations.wait_for_deferred_termination(owner) } {
            Ok(termination) => termination,
            Err(error) => {
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination wait",
                    &XllError::Internal {
                        diagnostic_id: match error {
                            ServerCloseError::WaitFailed(status) => {
                                crate::diagnostics::id::DiagnosticId::RTD_WINDOW_STATUS
                                    .with_low_u32(status as u32)
                            }
                            _ => crate::diagnostics::id::DiagnosticId::RTD_WINDOW_FAILURE,
                        },
                    },
                );
                // A live owned event cannot fail under normal operation. Once
                // ServerTerminate returned S_OK, reopening would strand a
                // completed JoinHandle on a reusable server.
                std::process::abort();
            }
        };

        #[cfg(test)]
        if PANIC_DEFERRED_TERMINATION_CLEANUP.swap(false, Ordering::AcqRel) {
            panic!("injected deferred RTD cleanup panic");
        }

        // Deferred cleanup intentionally retains ACTIVE_SERVER. The next
        // xlAutoRemove or ensure_server joins this worker before removing that
        // final module-lifetime reference.
        // SAFETY: `reference` and `termination` keep the server live/quiescent.
        let status = unsafe { teardown_server_resources(this, false) };
        if status != S_OK {
            crate::diagnostics::report_no_unwind(
                "IRtdServer::deferred termination",
                &XllError::WindowsApi {
                    function: "IRtdServer::ServerTerminate",
                    code: status,
                },
            );
        }
        drop(termination);
    }));

    if let Err(payload) = outcome {
        crate::diagnostics::report_no_unwind("IRtdServer::deferred termination", &XllError::Panic);
        drop(reference);
        std::panic::resume_unwind(payload);
    }
    drop(reference);
}

pub(super) unsafe fn teardown_server_resources(this: *mut RtdServer, remove_active: bool) -> i32 {
    let (handles, subscription_server) = {
        // SAFETY: `this` is non-null when entering server teardown.
        let backends = unsafe { (*this).backends.lock() };
        (backends.handles, backends.subscription_server)
    };

    // SAFETY: `this` is non-null when entering server teardown.
    let generation = unsafe { (*this).generation };

    let termination_status = match subscription_server.as_ref() {
        Some(subscription_server) => match subscription_server.terminate() {
            Ok(()) => S_OK,
            Err(_) => E_FAIL,
        },
        None => S_OK,
    };

    if let Some(handles) = handles {
        handles.terminate_topics(lifetime_generation(generation));
    }

    // SAFETY: the caller retains the server through callback revocation.
    unsafe { drain_callbacks(&(*this).callbacks) };

    if remove_active {
        let pointer = this as usize;
        let owned = {
            let mut active = ACTIVE_SERVER.lock();
            active
                .as_ref()
                .is_some_and(|entry| entry.pointer == pointer)
                .then(|| active.take())
                .flatten()
                .is_some()
        };

        if owned {
            // SAFETY: removing the server from ACTIVE_SERVER transfers its
            // owned reference to this branch, released exactly once.
            unsafe { server_release(this) };
        }
    }

    termination_status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_generation_allocator_refuses_wrap_without_mutating_after_exhaustion() {
        let counter = AtomicU64::new(u64::MAX - 1);

        assert_eq!(
            allocate_server_generation(&counter),
            ServerGeneration::new(u64::MAX)
        );
        assert_eq!(counter.load(Ordering::Acquire), u64::MAX);

        assert_eq!(allocate_server_generation(&counter), None);
        assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
    }
}
