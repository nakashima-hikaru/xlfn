use super::HandleRuntime;
use crate::RtdValue;
use crate::host_callback::HostCallbackSession;
use crate::subscription::{RtdUpdate, SubscriptionRuntime};
use crate::win32::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, CO_E_SERVER_STOPPING, CloseHandle,
    CoCreateGuid, CreateMutexW, DISP_E_BADINDEX, DISP_E_BADPARAMCOUNT, DISP_E_MEMBERNOTFOUND,
    DISP_E_PARAMNOTFOUND, DISP_E_TYPEMISMATCH, DISP_E_UNKNOWNINTERFACE, DISP_E_UNKNOWNNAME,
    DISPATCH_METHOD, DISPID_UNKNOWN, DISPPARAMS, E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL,
    E_OUTOFMEMORY, E_POINTER, E_UNEXPECTED, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, EXCEPINFO, GUID, GetLastError, HANDLE, HKEY, HKEY_CURRENT_USER, INFINITE,
    KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, ReleaseMutex,
    S_OK, SAFEARRAY, SAFEARRAYBOUND, SafeArrayCreate, SafeArrayDestroy, SafeArrayGetDim,
    SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype,
    SafeArrayPutElement, SysAllocStringLen, SysFreeString, SysStringLen, VARIANT, VARIANT_BOOL,
    VARIANT_FALSE, VARIANT_TRUE, VT_ARRAY, VT_BOOL, VT_BSTR, VT_BYREF, VT_DISPATCH, VT_EMPTY,
    VT_ERROR, VT_I4, VT_R8, VT_UNKNOWN, VT_VARIANT, VariantClear, WAIT_ABANDONED, WAIT_FAILED,
    WAIT_OBJECT_0, WaitForSingleObject,
};
use crate::{ExcelCallbackStatus, FromExcel, InputError, OwnedExcelValue, XllError, XllResult};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::ThreadId;
use xlfn_sys::{XL_GET_NAME, XLF_RTD, XLOPER12, XLOPER12Value, XLTYPE_STR};

mod com_abi;
mod event;
mod global_interface_table;
mod module_state;
mod server_gate;
mod update_event;
use com_abi::{IID_IUNKNOWN, IUnknown_Vtbl};
use global_interface_table::get_git;
use module_state::{COM_MODULE_LIFETIME, ComObjectKind, ComObjectLease};
use server_gate::{
    ServerCloseError, ServerOperationBarrier, ServerPhase, ServerTerminationRequest,
    TerminationWorker,
};
#[cfg(test)]
use server_gate::{
    ServerNotificationOperation, ServerOperation, ServerTermination, TerminationWorkerStatus,
};
#[cfg(test)]
use update_event::retry_git_revocation_debt_with;
use update_event::{
    GitCookieLease, OwnedRtdUpdateEvent, RetainedUpdateCallback, RtdUpdateEvent, ServerCallbacks,
    active_callback, drain_callbacks, install_callback, notification_for,
    retry_git_revocation_debt,
};

const IID_ICLASS_FACTORY: GUID = GUID::from_u128(0x0000_0001_0000_0000_c000_0000_0000_0046);
const IID_IDISPATCH: GUID = GUID::from_u128(0x0002_0400_0000_0000_c000_0000_0000_0046);
const IID_NULL: GUID = GUID::from_u128(0);
const IID_IRTD_SERVER: GUID = GUID::from_u128(0xec0e6191_db51_11d3_8f3e_00c04f3651b8);

const DISPID_SERVER_START: i32 = 10;
const DISPID_CONNECT_DATA: i32 = 11;
const DISPID_REFRESH_DATA: i32 = 12;
const DISPID_DISCONNECT_DATA: i32 = 13;
const DISPID_HEARTBEAT: i32 = 14;
const DISPID_SERVER_TERMINATE: i32 = 15;

const IID_IRTD_UPDATE_EVENT: GUID = GUID::from_u128(0xa43788c1_d91b_11d3_8f39_00c04f3651b8);
const RTD_REGISTRATION_OWNER: &str = "xlfn";
// Schema 2 registrations are protected by one cross-process mutex for the
// complete temporary registration lifetime. Do not scavenge schema 1 entries:
// an older live XLL does not participate in that protocol, so its registration
// cannot be proven stale.
const RTD_REGISTRATION_SCHEMA: &str = "2";
const RTD_PROG_ID_PREFIX: &str = "XlFnRtd_";
const SERVER_NOT_STARTED: u8 = 0;
const SERVER_STARTING: u8 = 1;
const SERVER_STARTED: u8 = 2;
const SERVER_START_FAILED: u8 = 3;
static REGISTRATION_MAINTENANCE: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct ActiveServer {
    class_id: GUID,
    prog_id: String,
    pointer: usize,
    generation: u64,
}

static ACTIVE_SERVER: Mutex<Option<ActiveServer>> = Mutex::new(None);
static NEXT_SERVER_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
static PANIC_IN_REFRESH_DATA: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static FAIL_DEFERRED_TERMINATION_SPAWN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static PANIC_DEFERRED_TERMINATION_CLEANUP: AtomicBool = AtomicBool::new(false);

struct EnsuredServer {
    active: ActiveServer,
    newly_created: bool,
    subscription_server: Option<crate::subscription::RtdServerHandle>,
}

impl Drop for EnsuredServer {
    fn drop(&mut self) {
        // SAFETY: `ensure_server` acquires one temporary reference specifically
        // for this guard, so dropping the guard must release exactly that reference.
        unsafe { server_release(self.active.pointer as *mut RtdServer) };
    }
}

#[repr(C)]
struct ClassFactory {
    vtable: *const ClassFactoryVtable,
    references: AtomicU32,
    server: *mut RtdServer,
    // Keep the module hold until every other field has been destroyed.
    _module_lease: ComObjectLease,
}

#[repr(C)]
struct ClassFactoryVtable {
    query_interface:
        unsafe extern "system" fn(*mut ClassFactory, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    release: unsafe extern "system" fn(*mut ClassFactory) -> u32,
    create_instance: unsafe extern "system" fn(
        *mut ClassFactory,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> i32,
    lock_server: unsafe extern "system" fn(*mut ClassFactory, i32) -> i32,
}

#[repr(C)]
struct RtdServer {
    vtable: *const RtdServerVtable,
    references: AtomicU32,
    start_state: AtomicU8,
    generation: u64,
    operations: Arc<ServerOperationBarrier>,
    termination_worker: TerminationWorker,
    backends: Mutex<ServerBackends>,
    callbacks: Mutex<ServerCallbacks>,
    // Keep the module hold until every other field has been destroyed.
    _module_lease: ComObjectLease,
}

struct ServerStartReservation<'a> {
    state: &'a AtomicU8,
    committed: bool,
    rollback_state: u8,
}

impl<'a> ServerStartReservation<'a> {
    fn acquire(state: &'a AtomicU8) -> Option<Self> {
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

    fn callback_published(&mut self) {
        // Once a GIT cookie is server-owned, a later failure must be terminal
        // for this server instance. Allowing another ServerStart would retain
        // one more external COM reference on every failed retry.
        self.rollback_state = SERVER_START_FAILED;
    }

    fn commit(mut self) {
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
    pointer: usize,
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

struct ServerBackends {
    handles: Option<Arc<HandleRuntime>>,
    subscriptions: Option<Arc<SubscriptionRuntime>>,
    subscription_server: Option<crate::subscription::RtdServerHandle>,
}

fn synchronize_callback_notification(
    server: &RtdServer,
    callback: Arc<RetainedUpdateCallback>,
) -> XllResult<()> {
    let subscription_server = server.backends.lock().subscription_server.clone();
    let Some(subscription_server) = subscription_server else {
        return Ok(());
    };

    subscription_server
        .attach_update_callback(notification_for(callback, Arc::clone(&server.operations)))?;
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
        let subscription_server = self.backends.lock().subscription_server.clone();
        if let Some(subscription_server) = subscription_server {
            subscription_server.detach_update_callback();
        }

        drain_callbacks(&self.callbacks);
    }
}

#[repr(C)]
struct RtdServerVtable {
    query_interface:
        unsafe extern "system" fn(*mut RtdServer, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut RtdServer) -> u32,
    release: unsafe extern "system" fn(*mut RtdServer) -> u32,
    get_type_info_count: unsafe extern "system" fn(*mut RtdServer, *mut u32) -> i32,
    get_type_info: unsafe extern "system" fn(*mut RtdServer, u32, u32, *mut *mut c_void) -> i32,
    get_ids_of_names: unsafe extern "system" fn(
        *mut RtdServer,
        *const GUID,
        *const *const u16,
        u32,
        u32,
        *mut i32,
    ) -> i32,
    invoke: unsafe extern "system" fn(
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
    server_start: unsafe extern "system" fn(*mut RtdServer, *mut c_void, *mut i32) -> i32,
    connect_data: unsafe extern "system" fn(
        *mut RtdServer,
        i32,
        *mut *mut SAFEARRAY,
        *mut VARIANT_BOOL,
        *mut VARIANT,
    ) -> i32,
    refresh_data: unsafe extern "system" fn(*mut RtdServer, *mut i32, *mut *mut SAFEARRAY) -> i32,
    disconnect_data: unsafe extern "system" fn(*mut RtdServer, i32) -> i32,
    heartbeat: unsafe extern "system" fn(*mut RtdServer, *mut i32) -> i32,
    server_terminate: unsafe extern "system" fn(*mut RtdServer) -> i32,
}

static CLASS_FACTORY_VTABLE: ClassFactoryVtable = ClassFactoryVtable {
    query_interface: factory_query_interface,
    add_ref: factory_add_ref,
    release: factory_release,
    create_instance: factory_create_instance,
    lock_server: factory_lock_server,
};

static RTD_SERVER_VTABLE: RtdServerVtable = RtdServerVtable {
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

pub(super) fn observe(
    handles: Arc<HandleRuntime>,
    key: &str,
    token: &str,
    callbacks: &HostCallbackSession,
) -> XllResult<()> {
    let _rtd_operation = handles.begin_rtd_operation()?;
    let ensured = ensure_server(Some(Arc::clone(&handles)), None)?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout `observe`.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(callbacks) {
            Ok(path) => path,
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        };

        match TemporaryRegistration::new(active, &module_path) {
            Ok(registration) => Some(registration),
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        }
    };

    let mut prog_id = match CountedString::new(&active.prog_id) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let mut topic = match CountedString::new(&format!("handle:{key}")) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    if let Err(error) = handles.claim_server(key, active.generation) {
        discard_unpublished_server(active.pointer, ensured.newly_created);
        return Err(error);
    }

    let mut server_name = XLOPER12::missing();
    let arguments = [
        prog_id.pointer(),
        NonNull::from(&mut server_name),
        topic.pointer(),
    ];

    // SAFETY: every pointer in `arguments` refers to a live XLOPER12 that
    // remains valid and stationary for the duration of the Excel callback.
    let (status, mut result) = unsafe {
        callbacks
            .call(XLF_RTD, &arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlfRtd(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };

    drop(registration);

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlfRtd",
            code: status.raw_code(),
        }));
    }

    let returned = String::from_excel(
        result.borrow()?,
        "RTD handle",
        &crate::CallContext::without_runtime(),
    )?;
    result.try_release()?;

    if returned != token {
        return Err(XllError::Internal {
            diagnostic_id: 0x5254_4448_414e_444c,
        });
    }

    Ok(())
}

pub(super) fn observe_subscription(
    subscriptions: Arc<SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    callbacks: &HostCallbackSession,
) -> XllResult<RtdValue> {
    let _rtd_operation = subscriptions.enter_external_operation()?;
    let ensured = ensure_server(None, Some(Arc::clone(&subscriptions)))?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout this function.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(callbacks) {
            Ok(path) => path,
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        };

        match TemporaryRegistration::new(active, &module_path) {
            Ok(registration) => Some(registration),
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        }
    };

    let mut prog_id = match CountedString::new(&active.prog_id) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let mut topic = match CountedString::new(key.as_str()) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    if let Some(subscription_server) = &ensured.subscription_server
        && let Err(error) = subscription_server.claim(key)
    {
        discard_unpublished_server(active.pointer, ensured.newly_created);
        return Err(error);
    }

    let mut server_name = XLOPER12::missing();
    let arguments = [
        prog_id.pointer(),
        NonNull::from(&mut server_name),
        topic.pointer(),
    ];

    // SAFETY: every pointer in `arguments` refers to a live XLOPER12 that
    // remains valid and stationary for the duration of the Excel callback.
    let (status, mut result) = unsafe {
        callbacks
            .call(XLF_RTD, &arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlfRtd(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };

    drop(registration);

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlfRtd",
            code: status.raw_code(),
        }));
    }

    let value = OwnedExcelValue::from_excel(
        result.borrow()?,
        "RTD value",
        &crate::CallContext::without_runtime(),
    )?;
    result.try_release()?;

    RtdValue::try_from(value)
}

fn discard_unpublished_server(pointer: usize, newly_created: bool) {
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

pub(super) fn shutdown(handles: Arc<HandleRuntime>) -> XllResult<()> {
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
                        .is_some_and(|active| Arc::ptr_eq(active, &handles))
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
        handles.terminate_topics(retained.generation);
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

pub(super) fn shutdown_subscriptions(subscriptions: Arc<SubscriptionRuntime>) -> XllResult<()> {
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
                        .is_some_and(|active| Arc::ptr_eq(active, &subscriptions))
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

fn ensure_server(
    handles: Option<Arc<HandleRuntime>>,
    subscriptions: Option<Arc<SubscriptionRuntime>>,
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
            return ensure_server(handles, subscriptions);
        }

        // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
        // held. A closing server cannot accept a newly attached backend.
        let _server_operation = unsafe { (*server).operations.enter() }.ok_or(XllError::Closing)?;

        // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
        // held, so the RtdServer and its `backends` mutex are valid.
        let mut backends = unsafe { (*server).backends.lock() };

        if let Some(handles) = handles {
            match backends.handles.as_ref() {
                Some(active) if Arc::ptr_eq(active, &handles) => {}
                Some(_) => {
                    return Err(XllError::Internal {
                        diagnostic_id: 0x5254_444d_554c_5449,
                    });
                }
                None => backends.handles = Some(handles),
            }
        }

        let (newly_attached_subscriptions, subscription_handle) =
            if let Some(subscriptions) = subscriptions {
                match backends.subscriptions.as_ref() {
                    Some(active) if Arc::ptr_eq(active, &subscriptions) => {
                        (None, backends.subscription_server.clone())
                    }
                    Some(_) => {
                        return Err(XllError::Internal {
                            diagnostic_id: 0x5254_444d_554c_5449,
                        });
                    }
                    None => {
                        let handle = subscriptions.register_server(
                            crate::subscription::ServerGeneration(existing.generation),
                        )?;
                        backends.subscriptions = Some(Arc::clone(&subscriptions));
                        backends.subscription_server = Some(handle.clone());
                        (Some(handle.clone()), Some(handle))
                    }
                }
            } else {
                (None, backends.subscription_server.clone())
            };

        drop(backends);

        if let Some(handle) = newly_attached_subscriptions {
            // SAFETY: `server` was validated as non-null and COM keeps the server alive.
            let callback = unsafe { active_callback(&(*server).callbacks) };
            if let Some(callback) = callback {
                handle.attach_update_callback(notification_for(callback, operations.clone()))?;
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
        return Err(XllError::ExcelApi {
            function: "CoCreateGuid",
            code: status,
        });
    }

    let generation = NEXT_SERVER_GENERATION.fetch_add(1, Ordering::Relaxed);
    let operations = ServerOperationBarrier::new().map_err(|error| XllError::ExcelApi {
        function: error.operation,
        code: error.code as i32,
    })?;

    let subscription_handle = if let Some(subscriptions) = subscriptions.as_ref() {
        Some(subscriptions.register_server(crate::subscription::ServerGeneration(generation))?)
    } else {
        None
    };

    let server = Box::new(RtdServer {
        vtable: &RTD_SERVER_VTABLE,
        references: AtomicU32::new(1),
        start_state: AtomicU8::new(SERVER_NOT_STARTED),
        generation,
        operations: Arc::new(operations),
        termination_worker: TerminationWorker::default(),
        backends: Mutex::new(ServerBackends {
            handles,
            subscriptions,
            subscription_server: subscription_handle.clone(),
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

pub(super) unsafe fn dll_get_class_object(
    class_id: *const c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    com_boundary("DllGetClassObject", || {
        // SAFETY: the raw pointers are forwarded unchanged from the COM export.
        // The inner function validates them before dereferencing.
        unsafe { dll_get_class_object_inner(class_id, interface_id, output) }
    })
}

unsafe fn dll_get_class_object_inner(
    class_id: *const c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to a writable COM
    // output slot supplied by the caller.
    unsafe { *output = ptr::null_mut() };

    if class_id.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: COM supplied a readable pointer to a GUID and it was validated as
    // non-null above.
    let requested_class = unsafe { *(class_id.cast::<GUID>()) };

    let active = ACTIVE_SERVER.lock();
    let Some(entry) = active
        .as_ref()
        .filter(|entry| guid_eq(entry.class_id, requested_class))
    else {
        return CLASS_E_CLASSNOTAVAILABLE;
    };

    let server = entry.pointer as *mut RtdServer;

    // Do not publish a new class factory once add-in shutdown has closed this
    // server. Holding the guard also makes a concurrent shutdown wait until the
    // factory owns its server reference.
    // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is held.
    let _server_operation = match unsafe { (*server).operations.enter() } {
        Some(operation) => operation,
        None => return CLASS_E_CLASSNOTAVAILABLE,
    };

    // SAFETY: ACTIVE_SERVER owns a live server reference. The factory receives
    // an additional reference that it releases when the factory is destroyed.
    unsafe { server_add_ref(server) };

    let factory = Box::into_raw(Box::new(ClassFactory {
        vtable: &CLASS_FACTORY_VTABLE,
        references: AtomicU32::new(1),
        server,
        _module_lease: ComObjectLease::new(ComObjectKind::Factory),
    }));

    // SAFETY: `factory` is a newly allocated live COM object, `interface_id` is
    // a validated readable GUID pointer, and `output` is writable.
    let status = unsafe { factory_query_interface(factory, interface_id.cast::<GUID>(), output) };

    // SAFETY: release the construction reference. QueryInterface acquired a
    // separate reference if it succeeded.
    unsafe { factory_release(factory) };

    status
}

unsafe extern "system" fn factory_query_interface(
    this: *mut ClassFactory,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
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

    if guid_eq(interface_id, IID_IUNKNOWN) || guid_eq(interface_id, IID_ICLASS_FACTORY) {
        // SAFETY: `output` is writable and `this` is a live factory pointer.
        // AddRef creates the reference returned through `output`.
        unsafe {
            *output = this.cast();
            factory_add_ref(this);
        }

        S_OK
    } else {
        E_NOINTERFACE
    }
}

unsafe extern "system" fn factory_add_ref(this: *mut ClassFactory) -> u32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
    // SAFETY: COM calls AddRef only on a live object pointer. The atomic update
    // preserves the shared COM reference count.
    unsafe { (*this).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn factory_release(this: *mut ClassFactory) -> u32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
    let Some(this) = NonNull::new(this) else {
        return 0;
    };

    // SAFETY: COM calls Release only on a live object. A zero previous count
    // is an invariant violation and must never wrap to `u32::MAX`.
    let previous = unsafe { this.as_ref().references.fetch_sub(1, Ordering::AcqRel) };
    if previous == 0 {
        std::process::abort();
    }
    let remaining = previous - 1;

    if remaining == 0 {
        // SAFETY: observing the transition to zero proves this is the final
        // reference and uniquely owns the original Box allocation.
        let factory = unsafe { Box::from_raw(this.as_ptr()) };

        // SAFETY: each factory owns one server reference acquired when the
        // factory was constructed.
        unsafe { server_release(factory.server) };
    }

    remaining
}

unsafe extern "system" fn factory_create_instance(
    this: *mut ClassFactory,
    outer: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if !output.is_null() {
        // SAFETY: COM supplied `output` as the writable result slot for this
        // method. Clearing it before the panic boundary guarantees every
        // failure path, including an unexpected unwind, returns no stale
        // interface pointer.
        unsafe { *output = ptr::null_mut() };
    }

    com_boundary("IClassFactory::CreateInstance", || {
        // SAFETY: the raw arguments are forwarded from the COM vtable method.
        // The inner function validates all applicable pointer contracts.
        unsafe { factory_create_instance_inner(this, outer, interface_id, output) }
    })
}

unsafe fn factory_create_instance_inner(
    this: *mut ClassFactory,
    outer: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }

    // SAFETY: `output` was validated as non-null and points to the writable
    // result slot supplied by COM.
    unsafe { *output = ptr::null_mut() };

    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }

    if this.is_null() || interface_id.is_null() {
        return E_POINTER;
    }

    // SAFETY: `this` was validated as non-null, the live factory owns one server
    // reference, and therefore its server pointer remains valid for this call.
    let server = unsafe { (*this).server };
    // SAFETY: the live factory reference described above keeps `server` valid
    // while the operation guard is acquired and held.
    let _server_operation = match unsafe { (*server).operations.enter() } {
        Some(operation) => operation,
        None => return E_FAIL,
    };

    // SAFETY: `this` was validated as non-null and COM keeps the factory live
    // during this call. server_query_interface validates the forwarded interface
    // and output pointers before dereferencing them.
    unsafe { server_query_interface(server, interface_id, output) }
}

unsafe extern "system" fn factory_lock_server(this: *mut ClassFactory, lock: i32) -> i32 {
    let operation = || {
        if this.is_null() {
            return E_POINTER;
        }
        if COM_MODULE_LIFETIME.set_server_lock(lock != 0) {
            S_OK
        } else {
            E_UNEXPECTED
        }
    };

    if lock == 0 {
        // Unlocking releases an existing module hold rather than admitting new
        // work. It must remain available after ingress enters CLOSING.
        let (_module_call, _accepted) = COM_MODULE_LIFETIME.enter_call();
        match catch_unwind(AssertUnwindSafe(operation)) {
            Ok(status) => status,
            Err(_) => {
                crate::diagnostics::report_no_unwind("IClassFactory::LockServer", &XllError::Panic);
                E_UNEXPECTED
            }
        }
    } else {
        // Acquiring another server lock is new work and remains subject to
        // normal ingress admission.
        com_boundary("IClassFactory::LockServer", operation)
    }
}

unsafe extern "system" fn server_query_interface(
    this: *mut RtdServer,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
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

unsafe extern "system" fn server_add_ref(this: *mut RtdServer) -> u32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
    // SAFETY: COM and internal callers invoke AddRef only on a live RtdServer.
    unsafe { (*this).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn server_release(this: *mut RtdServer) -> u32 {
    let _module_call = COM_MODULE_LIFETIME.enter_call();
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
    let _module_call = COM_MODULE_LIFETIME.enter_call();
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
    let _module_call = COM_MODULE_LIFETIME.enter_call();
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

unsafe extern "system" fn server_get_ids_of_names(
    this: *mut RtdServer,
    riid: *const GUID,
    names: *const *const u16,
    count: u32,
    _locale: u32,
    ids: *mut i32,
) -> i32 {
    com_boundary("IDispatch::GetIDsOfNames", || {
        // SAFETY: raw COM arguments are validated before the inner function
        // dereferences them.
        unsafe { server_get_ids_of_names_inner(this, riid, names, count, ids) }
    })
}

unsafe fn server_get_ids_of_names_inner(
    this: *mut RtdServer,
    riid: *const GUID,
    names: *const *const u16,
    count: u32,
    ids: *mut i32,
) -> i32 {
    if this.is_null() || riid.is_null() || names.is_null() || ids.is_null() {
        return E_POINTER;
    }

    if count == 0 {
        return E_INVALIDARG;
    }

    // SAFETY: `riid` was validated as non-null and points to the IID supplied
    // by COM for this call.
    if !guid_eq(unsafe { *riid }, IID_NULL) {
        return DISP_E_UNKNOWNINTERFACE;
    }

    // Initialize every result before inspecting the supplied names so partial
    // name-resolution failure never leaves stale DISPIDs behind.
    for index in 0..count as usize {
        // SAFETY: COM guarantees `ids` points to `count` writable elements.
        unsafe { *ids.add(index) = DISPID_UNKNOWN };
    }

    // SAFETY: `names` points to `count` readable name pointers.
    let member = unsafe { dispatch_member_id(*names) };
    // SAFETY: `ids` points to at least one writable element.
    unsafe { *ids = member.unwrap_or(DISPID_UNKNOWN) };

    let mut all_known = member.is_some();
    if let Some(member) = member {
        for index in 1..count as usize {
            // SAFETY: `names` and `ids` each point to `count` elements. Name
            // matching reads only the expected ASCII name length plus its NUL.
            let parameter = unsafe { dispatch_parameter_id(member, *names.add(index)) };
            // SAFETY: `ids` points to `count` writable elements.
            unsafe { *ids.add(index) = parameter.unwrap_or(DISPID_UNKNOWN) };
            all_known &= parameter.is_some();
        }
    }

    if all_known { S_OK } else { DISP_E_UNKNOWNNAME }
}

unsafe extern "system" fn server_invoke(
    this: *mut RtdServer,
    id: i32,
    riid: *const GUID,
    _locale: u32,
    flags: u16,
    parameters: *mut DISPPARAMS,
    result: *mut VARIANT,
    exception: *mut EXCEPINFO,
    argument_error: *mut u32,
) -> i32 {
    com_boundary("IDispatch::Invoke", || {
        // SAFETY: raw COM arguments are validated before the inner function
        // dereferences them.
        unsafe {
            server_invoke_inner(
                this,
                id,
                riid,
                flags,
                parameters,
                result,
                exception,
                argument_error,
            )
        }
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "COM IDispatch::Invoke ABI signature"
)]
unsafe fn server_invoke_inner(
    this: *mut RtdServer,
    id: i32,
    riid: *const GUID,
    flags: u16,
    parameters: *mut DISPPARAMS,
    result: *mut VARIANT,
    exception: *mut EXCEPINFO,
    argument_error: *mut u32,
) -> i32 {
    if !result.is_null() {
        // SAFETY: COM supplied an optional writable VARIANT result. The caller
        // initializes it before Invoke and transfers the empty slot to us.
        unsafe { ptr::write(result, VARIANT::default()) };
    }
    if !exception.is_null() {
        // SAFETY: COM supplied optional writable exception storage.
        unsafe { ptr::write(exception, EXCEPINFO::default()) };
    }
    if !argument_error.is_null() {
        // SAFETY: COM supplied optional writable argument-index storage.
        unsafe { *argument_error = 0 };
    }

    if this.is_null() || riid.is_null() || parameters.is_null() {
        return E_POINTER;
    }

    // SAFETY: `riid` was validated as non-null and points to a readable GUID.
    if !guid_eq(unsafe { *riid }, IID_NULL) {
        return DISP_E_UNKNOWNINTERFACE;
    }

    if flags & DISPATCH_METHOD == 0 {
        return DISP_E_MEMBERNOTFOUND;
    }

    let Some(expected_arguments) = dispatch_argument_count(id) else {
        return DISP_E_MEMBERNOTFOUND;
    };

    // SAFETY: `parameters` was validated as non-null and COM keeps the
    // DISPPARAMS and its argument arrays live throughout Invoke.
    let parameters = unsafe { &mut *parameters };
    // SAFETY: the DISPPARAMS pointer arrays are validated by the helper before
    // they are dereferenced.
    let arguments =
        match unsafe { collect_dispatch_arguments(parameters, expected_arguments, argument_error) }
        {
            Ok(arguments) => arguments,
            Err(status) => return status,
        };

    match id {
        DISPID_SERVER_START => {
            // SAFETY: argument zero was obtained from the validated DISPPARAMS.
            let Some(callback) = (unsafe { query_rtd_update_event(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut value = 0;
            // SAFETY: `callback` owns one queried IRTDUpdateEvent reference for
            // this call and `value` is writable.
            let status = unsafe { server_start(this, callback.as_ptr().cast(), &mut value) };

            if status == S_OK {
                // SAFETY: the optional result was initialized above.
                unsafe { write_i4_dispatch_result(result, value) };
            }
            status
        }
        DISPID_CONNECT_DATA => {
            // SAFETY: all argument pointers were obtained from validated
            // DISPPARAMS storage.
            let Some(topic_id) = (unsafe { dispatch_i4_value(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };
            // SAFETY: see above.
            let Some(strings) = (unsafe { dispatch_array_argument(arguments[1]) }) else {
                // SAFETY: argument one belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[1], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };
            // SAFETY: see above.
            let Some(new_values) = (unsafe { dispatch_bool_reference(arguments[2]) }) else {
                // SAFETY: argument two belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[2], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut by_value_array;
            let strings = match strings {
                DispatchArrayArgument::ByReference(strings) => strings.as_ptr(),
                DispatchArrayArgument::ByValue(strings) => {
                    by_value_array = strings.as_ptr();
                    &mut by_value_array
                }
            };
            let mut ignored_result = VARIANT::default();
            let result_slot = if result.is_null() {
                &mut ignored_result
            } else {
                result
            };

            // SAFETY: each typed argument was validated above and remains live
            // for the duration of the vtable call.
            let status =
                unsafe { connect_data(this, topic_id, strings, new_values.as_ptr(), result_slot) };

            if result.is_null() {
                // SAFETY: connect_data either left the initialized local empty
                // or populated it with a valid owned VARIANT.
                unsafe { VariantClear(&mut ignored_result) };
            }
            status
        }
        DISPID_REFRESH_DATA => {
            // SAFETY: argument zero was obtained from validated DISPPARAMS.
            let Some(topic_count) = (unsafe { dispatch_i4_reference(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            let mut array = ptr::null_mut();
            // SAFETY: `topic_count` and `array` are writable for the call.
            let status = unsafe { refresh_data(this, topic_count.as_ptr(), &mut array) };
            if status == S_OK && !result.is_null() {
                // SAFETY: the optional result was initialized above and takes
                // ownership of the returned SAFEARRAY.
                unsafe { write_array_dispatch_result(result, array) };
            } else if !array.is_null() {
                // SAFETY: no caller-visible VARIANT took ownership of the
                // SAFEARRAY, so this branch destroys it exactly once.
                unsafe { SafeArrayDestroy(array) };
            }
            status
        }
        DISPID_DISCONNECT_DATA => {
            // SAFETY: argument zero was obtained from validated DISPPARAMS.
            let Some(topic_id) = (unsafe { dispatch_i4_value(arguments[0]) }) else {
                // SAFETY: argument zero belongs to `parameters.rgvarg`.
                unsafe { set_dispatch_argument_error(parameters, arguments[0], argument_error) };
                return DISP_E_TYPEMISMATCH;
            };

            // SAFETY: the scalar argument was validated above.
            unsafe { disconnect_data(this, topic_id) }
        }
        DISPID_HEARTBEAT => {
            let mut value = 0;
            // SAFETY: `value` is writable for the typed RTD method.
            let status = unsafe { heartbeat(this, &mut value) };
            if status == S_OK {
                // SAFETY: the optional result was initialized above.
                unsafe { write_i4_dispatch_result(result, value) };
            }
            status
        }
        DISPID_SERVER_TERMINATE => {
            // SAFETY: `this` was validated above and COM holds it live for
            // Invoke.
            unsafe { server_terminate(this) }
        }
        _ => DISP_E_MEMBERNOTFOUND,
    }
}

fn dispatch_argument_count(id: i32) -> Option<usize> {
    match id {
        DISPID_SERVER_START | DISPID_REFRESH_DATA | DISPID_DISCONNECT_DATA => Some(1),
        DISPID_CONNECT_DATA => Some(3),
        DISPID_HEARTBEAT | DISPID_SERVER_TERMINATE => Some(0),
        _ => None,
    }
}

unsafe fn collect_dispatch_arguments(
    parameters: &mut DISPPARAMS,
    expected: usize,
    argument_error: *mut u32,
) -> Result<[*mut VARIANT; 3], i32> {
    let argument_count = parameters.cArgs as usize;
    let named_count = parameters.cNamedArgs as usize;

    if argument_count != expected || named_count > argument_count {
        return Err(DISP_E_BADPARAMCOUNT);
    }
    if expected != 0 && parameters.rgvarg.is_null() {
        return Err(E_POINTER);
    }
    if named_count != 0 && parameters.rgdispidNamedArgs.is_null() {
        return Err(E_POINTER);
    }

    let mut arguments: [*mut VARIANT; 3] = [ptr::null_mut(); 3];

    for named_index in 0..named_count {
        // SAFETY: both arrays contain at least `named_count` elements.
        let parameter_id = unsafe { *parameters.rgdispidNamedArgs.add(named_index) };
        let Ok(parameter_index) = usize::try_from(parameter_id) else {
            if !argument_error.is_null() {
                // SAFETY: `argument_error` is an optional writable output.
                unsafe { *argument_error = named_index as u32 };
            }
            return Err(DISP_E_PARAMNOTFOUND);
        };

        if parameter_index >= expected || !arguments[parameter_index].is_null() {
            if !argument_error.is_null() {
                // SAFETY: `argument_error` is an optional writable output.
                unsafe { *argument_error = named_index as u32 };
            }
            return Err(DISP_E_PARAMNOTFOUND);
        }

        // SAFETY: rgvarg contains `argument_count` elements and named
        // arguments occupy its first `named_count` entries.
        arguments[parameter_index] = unsafe { parameters.rgvarg.add(named_index) };
    }

    let mut value_index = named_count;
    for parameter_index in (0..expected).rev() {
        if arguments[parameter_index].is_null() {
            // SAFETY: exact argument-count validation and the named-argument
            // checks ensure each remaining positional element is consumed once.
            arguments[parameter_index] = unsafe { parameters.rgvarg.add(value_index) };
            value_index += 1;
        }
    }

    Ok(arguments)
}

unsafe fn set_dispatch_argument_error(
    parameters: &DISPPARAMS,
    argument: *mut VARIANT,
    argument_error: *mut u32,
) {
    if argument_error.is_null() {
        return;
    }

    // SAFETY: `argument` was selected from `parameters.rgvarg`, so both
    // pointers belong to the same allocation and offset_from is defined.
    let index = unsafe { argument.offset_from(parameters.rgvarg) };
    // SAFETY: the optional output is writable. A valid DISPPARAMS index is
    // non-negative and fits in u32 because cArgs itself is u32.
    unsafe { *argument_error = index as u32 };
}

unsafe fn dispatch_i4_value(argument: *mut VARIANT) -> Option<i32> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: `argument` is a readable VARIANT and the discriminant is checked
    // before selecting the corresponding union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == VT_I4 {
        // SAFETY: the checked discriminant selects lVal.
        Some(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.lVal })
    } else {
        None
    }
}

unsafe fn dispatch_i4_reference(argument: *mut VARIANT) -> Option<NonNull<i32>> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before selecting the by-reference
    // union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == (VT_BYREF | VT_I4) {
        // SAFETY: the checked discriminant selects plVal.
        NonNull::new(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.plVal })
    } else {
        None
    }
}

unsafe fn dispatch_bool_reference(argument: *mut VARIANT) -> Option<NonNull<VARIANT_BOOL>> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before selecting the by-reference
    // union field.
    if unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt } == (VT_BYREF | VT_BOOL) {
        // SAFETY: the checked discriminant selects pboolVal.
        NonNull::new(unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pboolVal })
    } else {
        None
    }
}

enum DispatchArrayArgument {
    ByValue(NonNull<SAFEARRAY>),
    ByReference(NonNull<*mut SAFEARRAY>),
}

unsafe fn dispatch_array_argument(argument: *mut VARIANT) -> Option<DispatchArrayArgument> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: `argument` is readable for the discriminant and matching union
    // field selected below.
    let variant_type = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt };
    match variant_type {
        value if value == (VT_ARRAY | VT_VARIANT) => {
            // SAFETY: the checked discriminant selects parray.
            let array = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.parray };
            NonNull::new(array).map(DispatchArrayArgument::ByValue)
        }
        value if value == (VT_BYREF | VT_ARRAY | VT_VARIANT) => {
            // SAFETY: the checked discriminant selects pparray.
            let array = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pparray };
            NonNull::new(array).map(DispatchArrayArgument::ByReference)
        }
        _ => None,
    }
}

unsafe fn query_rtd_update_event(argument: *mut VARIANT) -> Option<OwnedRtdUpdateEvent> {
    // SAFETY: the caller supplies a readable argument VARIANT.
    let argument = unsafe { unwrap_dispatch_variant(argument)? };
    // SAFETY: the discriminant is checked before reading its matching interface
    // pointer field.
    let variant_type = unsafe { (*argument.as_ptr()).Anonymous.Anonymous.vt };
    // SAFETY: each match arm reads only the union field selected by the checked
    // discriminant; by-reference arms also reject null before dereferencing.
    let unknown = unsafe {
        match variant_type {
            VT_DISPATCH => (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.pdispVal,
            VT_UNKNOWN => (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.punkVal,
            value if value == (VT_BYREF | VT_DISPATCH) => {
                let pointer = (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.ppdispVal;
                NonNull::new(pointer).map(|pointer| *pointer.as_ptr())?
            }
            value if value == (VT_BYREF | VT_UNKNOWN) => {
                let pointer = (*argument.as_ptr()).Anonymous.Anonymous.Anonymous.ppunkVal;
                NonNull::new(pointer).map(|pointer| *pointer.as_ptr())?
            }
            _ => return None,
        }
    };
    let unknown = NonNull::new(unknown)?;

    // SAFETY: every COM interface begins with a readable IUnknown vtable
    // pointer, and `unknown` was obtained from a matching VARIANT arm.
    let vtable = unsafe { *unknown.as_ptr().cast::<*const IUnknown_Vtbl>() };
    let vtable = NonNull::new(vtable as *mut IUnknown_Vtbl)?;
    let mut output = ptr::null_mut();

    // SAFETY: the queried object is live for the VARIANT argument's lifetime,
    // the IID is readable, and `output` is writable.
    let status = unsafe {
        ((*vtable.as_ptr()).QueryInterface)(unknown.as_ptr(), &IID_IRTD_UPDATE_EVENT, &mut output)
    };

    if status == S_OK {
        NonNull::new(output.cast::<RtdUpdateEvent>()).map(|pointer| {
            // SAFETY: QueryInterface returned S_OK and one owned
            // IRTDUpdateEvent reference in `output`.
            unsafe { OwnedRtdUpdateEvent::from_raw(pointer) }
        })
    } else {
        None
    }
}

unsafe fn unwrap_dispatch_variant(mut argument: *mut VARIANT) -> Option<NonNull<VARIANT>> {
    for _ in 0..4 {
        let argument_ptr = NonNull::new(argument)?;
        // SAFETY: the current argument is a readable VARIANT.
        if unsafe { (*argument_ptr.as_ptr()).Anonymous.Anonymous.vt } != (VT_BYREF | VT_VARIANT) {
            return Some(argument_ptr);
        }

        // SAFETY: the checked discriminant selects pvarVal.
        argument = unsafe {
            (*argument_ptr.as_ptr())
                .Anonymous
                .Anonymous
                .Anonymous
                .pvarVal
        };
    }
    None
}

unsafe fn write_i4_dispatch_result(result: *mut VARIANT, value: i32) {
    if result.is_null() {
        return;
    }

    // SAFETY: the optional result points to initialized writable VARIANT
    // storage and the discriminant matches the selected union field.
    unsafe {
        (*result).Anonymous.Anonymous.vt = VT_I4;
        (*result).Anonymous.Anonymous.Anonymous.lVal = value;
    }
}

unsafe fn write_array_dispatch_result(result: *mut VARIANT, array: *mut SAFEARRAY) {
    // SAFETY: the caller passes initialized writable VARIANT storage and
    // transfers ownership of `array` to it.
    unsafe {
        (*result).Anonymous.Anonymous.vt = VT_ARRAY | VT_VARIANT;
        (*result).Anonymous.Anonymous.Anonymous.parray = array;
    }
}

unsafe fn dispatch_member_id(name: *const u16) -> Option<i32> {
    // SAFETY: GetIDsOfNames supplies a NUL-terminated OLE string. Each helper
    // reads no further than its fixed ASCII candidate plus the terminator.
    unsafe {
        if wide_name_eq_ascii(name, b"ServerStart") {
            Some(DISPID_SERVER_START)
        } else if wide_name_eq_ascii(name, b"ConnectData") {
            Some(DISPID_CONNECT_DATA)
        } else if wide_name_eq_ascii(name, b"RefreshData") {
            Some(DISPID_REFRESH_DATA)
        } else if wide_name_eq_ascii(name, b"DisconnectData") {
            Some(DISPID_DISCONNECT_DATA)
        } else if wide_name_eq_ascii(name, b"Heartbeat") {
            Some(DISPID_HEARTBEAT)
        } else if wide_name_eq_ascii(name, b"ServerTerminate") {
            Some(DISPID_SERVER_TERMINATE)
        } else {
            None
        }
    }
}

unsafe fn dispatch_parameter_id(member: i32, name: *const u16) -> Option<i32> {
    // SAFETY: GetIDsOfNames supplies a NUL-terminated OLE string and each
    // comparison reads only its fixed candidate length plus the terminator.
    unsafe {
        match member {
            DISPID_SERVER_START => wide_name_eq_ascii(name, b"CallbackObject").then_some(0),
            DISPID_CONNECT_DATA => {
                if wide_name_eq_ascii(name, b"TopicID") {
                    Some(0)
                } else if wide_name_eq_ascii(name, b"Strings") {
                    Some(1)
                } else if wide_name_eq_ascii(name, b"GetNewValues") {
                    Some(2)
                } else {
                    None
                }
            }
            DISPID_REFRESH_DATA => wide_name_eq_ascii(name, b"TopicCount").then_some(0),
            DISPID_DISCONNECT_DATA => wide_name_eq_ascii(name, b"TopicID").then_some(0),
            DISPID_HEARTBEAT | DISPID_SERVER_TERMINATE => None,
            _ => None,
        }
    }
}

unsafe fn wide_name_eq_ascii(name: *const u16, expected: &[u8]) -> bool {
    if name.is_null() {
        return false;
    }

    for (index, expected) in expected.iter().copied().enumerate() {
        // SAFETY: the GetIDsOfNames contract supplies a readable NUL-terminated
        // string. This loop reads only the fixed expected-name prefix.
        let actual = unsafe { *name.add(index) };
        let folded = if (u16::from(b'A')..=u16::from(b'Z')).contains(&actual) {
            actual + u16::from(b'a' - b'A')
        } else {
            actual
        };
        if folded != u16::from(expected.to_ascii_lowercase()) {
            return false;
        }
    }

    // SAFETY: the caller-supplied name is NUL-terminated; checking this one
    // element establishes that it has no unmatched suffix.
    unsafe { *name.add(expected.len()) == 0 }
}

unsafe extern "system" fn server_start(
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
    let _subscription_server = unsafe { (*this).backends.lock().subscription_server.clone() };

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

    let callback = Arc::new(RetainedUpdateCallback {
        cookie: Some(cookie),
        #[cfg(test)]
        drop_hook: None,
    });

    // SAFETY: `this` was validated as non-null and COM keeps the server alive
    // for the duration of ServerStart.
    unsafe { install_callback(&(*this).callbacks, Arc::clone(&callback)) };
    start_reservation.callback_published();

    // SAFETY: `this` remains live through the COM call. Re-reading backends
    // after installing the callback closes the race with a concurrently
    // attached subscription runtime.
    if unsafe { synchronize_callback_notification(&*this, callback) }.is_err() {
        return E_FAIL;
    }

    start_reservation.commit();

    // SAFETY: `result` was validated as non-null and remains valid for the
    // duration of this COM method.
    unsafe { *result = 1 };

    S_OK
}

unsafe extern "system" fn connect_data(
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

enum ConnectDataTransaction {
    Handle(crate::handle::HandleConnection),
    Subscription(crate::subscription::SubscriptionConnection),
}

impl ConnectDataTransaction {
    fn value(&self) -> RtdValue {
        match self {
            Self::Handle(connection) => RtdValue::String(connection.token().to_owned()),
            Self::Subscription(connection) => connection.value().clone(),
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

    let (handles, subscription_server) = {
        // SAFETY: `this` was validated as non-null and COM retains the server during ConnectData.
        let backends = unsafe { (*this).backends.lock() };
        (
            backends.handles.clone(),
            backends.subscription_server.clone(),
        )
    };

    // SAFETY: `this` remains valid for the duration of ConnectData.
    let generation = unsafe { (*this).generation };

    let connection = if let Some(handle_key) = key.strip_prefix("handle:") {
        let Some(handles) = handles.as_ref() else {
            return E_FAIL;
        };

        match handles.connect_transaction(generation, topic_id, handle_key) {
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

        match subscription_server
            .connect_transaction(crate::subscription::TopicId(topic_id), &sub_key)
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
    let value = connection.value();

    // SAFETY: `result` was validated as non-null and points to writable VARIANT
    // storage supplied by COM. `value` remains readable for the call.
    let status = unsafe { write_value_variant(result, &value) };

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

unsafe extern "system" fn refresh_data(
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
    let subscription_server = unsafe { (*this).backends.lock().subscription_server.clone() };

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

unsafe extern "system" fn disconnect_data(this: *mut RtdServer, topic_id: i32) -> i32 {
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
        (
            backends.handles.clone(),
            backends.subscription_server.clone(),
        )
    };

    // SAFETY: `this` remains valid for the duration of DisconnectData.
    let generation = unsafe { (*this).generation };

    if let Some(handles) = handles {
        handles.disconnect(generation, topic_id);
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

unsafe extern "system" fn heartbeat(this: *mut RtdServer, result: *mut i32) -> i32 {
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
            let subscription_server =
                unsafe { (*this).backends.lock().subscription_server.clone() };
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

unsafe extern "system" fn server_terminate(this: *mut RtdServer) -> i32 {
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
                                0x5254_4457_0000_0000 | u64::from(status as u32)
                            }
                            _ => 0x5254_4457_4641_494c,
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
        // xlAutoClose or ensure_server joins this worker before removing that
        // final module-lifetime reference.
        // SAFETY: `reference` and `termination` keep the server live/quiescent.
        let status = unsafe { teardown_server_resources(this, false) };
        if status != S_OK {
            crate::diagnostics::report_no_unwind(
                "IRtdServer::deferred termination",
                &XllError::ExcelApi {
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

unsafe fn teardown_server_resources(this: *mut RtdServer, remove_active: bool) -> i32 {
    let (handles, subscription_server) = {
        // SAFETY: `this` is non-null when entering server teardown.
        let backends = unsafe { (*this).backends.lock() };
        (
            backends.handles.clone(),
            backends.subscription_server.clone(),
        )
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
        handles.terminate_topics(generation);
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

fn com_boundary(operation: &'static str, callback: impl FnOnce() -> i32) -> i32 {
    let (_module_call, accepted) = COM_MODULE_LIFETIME.enter_call();
    if !accepted {
        return CO_E_SERVER_STOPPING;
    }
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(status) => status,
        Err(_) => {
            crate::diagnostics::report_no_unwind(operation, &XllError::Panic);
            E_UNEXPECTED
        }
    }
}

#[cfg(any(test, feature = "shutdown-refinement"))]
pub(super) fn set_ghost(ghost: crate::shutdown_refinement::GhostHandle) {
    COM_MODULE_LIFETIME.set_ghost(ghost);
}

pub(super) fn dll_can_unload_now() -> i32 {
    if crate::rtd::module_unload_certified() && COM_MODULE_LIFETIME.can_unload_now() {
        S_OK
    } else {
        1 // S_FALSE
    }
}

pub(super) fn wait_for_module_quiescence() -> Result<(), crate::rtd::RtdQuiescenceError> {
    COM_MODULE_LIFETIME
        .wait_for_quiescence(retry_git_revocation_debt)
        .map_err(|error| crate::rtd::RtdQuiescenceError {
            outstanding_git_cookies: error.state.outstanding_git_cookies,
            revocation_debt: error.state.revocation_debt,
        })
}

unsafe fn write_bstr_variant(result: *mut VARIANT, value: &str) -> i32 {
    let wide =
        match crate::utf16::encode_bounded(value, "RTD value", crate::utf16::EXCEL_STRING_LIMIT) {
            Ok(wide) => wide,
            Err(_) => return E_INVALIDARG,
        };

    let length = match u32::try_from(wide.len()) {
        Ok(length) => length,
        Err(_) => return E_INVALIDARG,
    };

    // SAFETY: `wide` is readable for exactly `length` UTF-16 code units and
    // remains live for the duration of SysAllocStringLen.
    let bstr = unsafe { SysAllocStringLen(wide.as_ptr(), length) };

    if bstr.is_null() {
        return E_FAIL;
    }

    let mut variant = VARIANT::default();
    variant.Anonymous.Anonymous.vt = VT_BSTR;
    variant.Anonymous.Anonymous.Anonymous.bstrVal = bstr;

    // SAFETY: the caller guarantees that `result` points to writable VARIANT
    // output storage. Ownership of `bstr` transfers into the written VARIANT.
    unsafe { *result = variant };

    S_OK
}

unsafe fn write_value_variant(result: *mut VARIANT, value: &RtdValue) -> i32 {
    if result.is_null() {
        return E_POINTER;
    }

    let mut variant = VARIANT::default();

    match value {
        RtdValue::Number(value) => {
            variant.Anonymous.Anonymous.vt = VT_R8;
            variant.Anonymous.Anonymous.Anonymous.dblVal = *value;
        }
        RtdValue::Boolean(value) => {
            variant.Anonymous.Anonymous.vt = VT_BOOL;
            variant.Anonymous.Anonymous.Anonymous.boolVal = if *value { -1 } else { 0 };
        }
        RtdValue::Integer(value) => {
            variant.Anonymous.Anonymous.vt = VT_I4;
            variant.Anonymous.Anonymous.Anonymous.lVal = *value;
        }
        RtdValue::String(value) => {
            // SAFETY: `result` was validated as non-null above and `value`
            // remains readable for the duration of the call.
            return unsafe { write_bstr_variant(result, value) };
        }
        RtdValue::Error(value) => {
            variant.Anonymous.Anonymous.vt = VT_ERROR;
            variant.Anonymous.Anonymous.Anonymous.scode = 2000 + value.0.code();
        }
        RtdValue::Empty => {
            variant.Anonymous.Anonymous.vt = VT_EMPTY;
        }
    }

    // SAFETY: `result` was validated as non-null and points to writable VARIANT
    // storage supplied by the caller.
    unsafe { *result = variant };

    S_OK
}

unsafe fn write_refresh_data(
    topic_count: *mut i32,
    result: *mut *mut SAFEARRAY,
    updates: &[RtdUpdate],
) -> i32 {
    // SAFETY: the caller validated both output pointers as non-null and writable.
    unsafe {
        *topic_count = 0;
        *result = ptr::null_mut();
    }

    if updates.is_empty() {
        return S_OK;
    }

    let count = match u32::try_from(updates.len()) {
        Ok(count) => count,
        Err(_) => return E_INVALIDARG,
    };

    let mut bounds = [
        SAFEARRAYBOUND {
            cElements: 2,
            lLbound: 0,
        },
        SAFEARRAYBOUND {
            cElements: count,
            lLbound: 0,
        },
    ];

    // SAFETY: `bounds` describes a valid two-dimensional SAFEARRAY of VARIANTs
    // and remains readable for the duration of SafeArrayCreate.
    let array = unsafe { SafeArrayCreate(VT_VARIANT, 2, bounds.as_mut_ptr()) };

    if array.is_null() {
        return E_OUTOFMEMORY;
    }

    for (column, update) in updates.iter().enumerate() {
        let Ok(column) = i32::try_from(column) else {
            // SAFETY: `array` was allocated above, has not been transferred to
            // Excel, and is destroyed exactly once on this error path.
            unsafe { SafeArrayDestroy(array) };
            return E_INVALIDARG;
        };

        let mut topic = VARIANT::default();
        topic.Anonymous.Anonymous.vt = VT_I4;
        topic.Anonymous.Anonymous.Anonymous.lVal = update.topic_id;

        let mut value_variant = VARIANT::default();

        // SAFETY: `value_variant` is initialized writable VARIANT storage owned
        // by this stack frame and `update.value` remains readable.
        let value_status = unsafe { write_value_variant(&mut value_variant, &update.value) };

        if value_status != S_OK {
            // SAFETY: `value_variant` is initialized and locally owned.
            // `array` has not been transferred to Excel and is destroyed once.
            unsafe {
                VariantClear(&mut value_variant);
                SafeArrayDestroy(array);
            }

            return value_status;
        }

        // The SAFEARRAY consists of two rows and one column per RTD update.
        // The first row stores topic IDs and the second row stores values.
        let mut topic_index = [0, column];
        let mut value_index = [1, column];

        // SAFETY: `topic_index` is within the declared two-dimensional bounds,
        // and `topic` points to a readable initialized VARIANT.
        let topic_status = unsafe {
            SafeArrayPutElement(
                array,
                topic_index.as_mut_ptr(),
                (&mut topic as *mut VARIANT).cast(),
            )
        };

        // SAFETY: `value_index` is within the declared two-dimensional bounds,
        // and `value_variant` points to a readable initialized VARIANT.
        let value_status = unsafe {
            SafeArrayPutElement(
                array,
                value_index.as_mut_ptr(),
                (&mut value_variant as *mut VARIANT).cast(),
            )
        };

        // SAFETY: SafeArrayPutElement copies VARIANT payloads. The local
        // VARIANTs remain owned here and must be cleared exactly once.
        unsafe {
            VariantClear(&mut topic);
            VariantClear(&mut value_variant);
        }

        if topic_status < 0 {
            // SAFETY: array has not been transferred to Excel.
            unsafe { SafeArrayDestroy(array) };
            return topic_status;
        }

        if value_status < 0 {
            // SAFETY: array has not been transferred to Excel.
            unsafe { SafeArrayDestroy(array) };
            return value_status;
        }
    }

    // SAFETY: both output pointers were validated as writable. Ownership of the
    // fully initialized SAFEARRAY transfers to Excel through `result`.
    unsafe {
        *topic_count = updates.len() as i32;
        *result = array;
    }

    S_OK
}

const MAX_RTD_TOPIC_PARTS: usize = 253;
const REQUIRED_RTD_TOPIC_PARTS: usize = 1;

fn checked_topic_part_count(lower: i32, upper: i32) -> XllResult<usize> {
    if upper < lower {
        return Err(XllError::InvalidHandle);
    }
    let count = i64::from(upper)
        .checked_sub(i64::from(lower))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            XllError::input(
                "RTD topic",
                InputError::TooLarge {
                    limit: MAX_RTD_TOPIC_PARTS,
                    actual: usize::MAX,
                },
            )
        })?;
    let count = usize::try_from(count).map_err(|_| {
        XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: MAX_RTD_TOPIC_PARTS,
                actual: usize::MAX,
            },
        )
    })?;
    if count == 0 || count > MAX_RTD_TOPIC_PARTS {
        return Err(XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: MAX_RTD_TOPIC_PARTS,
                actual: count,
            },
        ));
    }
    Ok(count)
}

fn checked_topic_part_length(length: usize) -> XllResult<()> {
    if length > crate::utf16::EXCEL_STRING_LIMIT {
        Err(XllError::input(
            "RTD topic",
            InputError::TooLarge {
                limit: crate::utf16::EXCEL_STRING_LIMIT,
                actual: length,
            },
        ))
    } else {
        Ok(())
    }
}

unsafe fn topic_key_from_safearray(strings: *mut *mut SAFEARRAY) -> XllResult<String> {
    let Some(strings) = NonNull::new(strings) else {
        return Err(XllError::InvalidHandle);
    };

    // SAFETY: `strings` is a validated non-null pointer to the SAFEARRAY pointer
    // supplied by COM and is readable for this method invocation.
    let array = unsafe { *strings.as_ptr() };

    let Some(array) = NonNull::new(array) else {
        return Err(XllError::InvalidHandle);
    };

    let array_ptr = array.as_ptr();

    // SAFETY: `array_ptr` is a non-null live SAFEARRAY pointer supplied by COM.
    let dim = unsafe { SafeArrayGetDim(array_ptr) };

    if dim != 1 {
        return Err(XllError::InvalidHandle);
    }

    let mut lower = 0;
    let mut upper = 0;

    // SAFETY: `array_ptr` is a live one-dimensional SAFEARRAY and `lower` is
    // writable output storage.
    let lower_status = unsafe { SafeArrayGetLBound(array_ptr, 1, &mut lower) };

    // SAFETY: `array_ptr` is a live one-dimensional SAFEARRAY and `upper` is
    // writable output storage.
    let upper_status = unsafe { SafeArrayGetUBound(array_ptr, 1, &mut upper) };

    if lower_status < 0 || upper_status < 0 {
        return Err(XllError::InvalidHandle);
    }
    let count = checked_topic_part_count(lower, upper)?;
    if count != REQUIRED_RTD_TOPIC_PARTS {
        return Err(XllError::InvalidHandle);
    }

    let mut vt = 0u16;

    // SAFETY: `array_ptr` is a live SAFEARRAY and `vt` is writable output
    // storage for its element VARTYPE.
    let vt_status = unsafe { SafeArrayGetVartype(array_ptr, &mut vt) };

    if vt_status < 0 {
        return Err(XllError::InvalidHandle);
    }

    let mut parts = Vec::with_capacity(count);

    for offset in 0..count {
        let index = i64::from(lower)
            .checked_add(i64::try_from(offset).map_err(|_| XllError::InvalidHandle)?)
            .and_then(|index| i32::try_from(index).ok())
            .ok_or(XllError::InvalidHandle)?;
        let part_str = match vt {
            VT_BSTR => {
                let mut bstr_ptr: *mut u16 = ptr::null_mut();

                // SAFETY: `index` is within the validated SAFEARRAY bounds and
                // `bstr_ptr` points to writable storage. For a VT_BSTR array,
                // SafeArrayGetElement returns a copied BSTR owned by the caller.
                if unsafe {
                    SafeArrayGetElement(array_ptr, &index, (&mut bstr_ptr as *mut *mut u16).cast())
                } < 0
                {
                    return Err(XllError::InvalidHandle);
                }

                let Some(bstr) = NonNull::new(bstr_ptr) else {
                    return Err(XllError::InvalidHandle);
                };

                let _bstr_guard = BstrGuard(bstr);

                // SAFETY: `bstr` is a live BSTR returned by
                // SafeArrayGetElement and remains live while `_bstr_guard` exists.
                let length = unsafe { SysStringLen(bstr.as_ptr()) } as usize;
                checked_topic_part_length(length)?;

                // SAFETY: a BSTR contains at least SysStringLen UTF-16 code
                // units, and `_bstr_guard` keeps the allocation alive.
                let units = unsafe { std::slice::from_raw_parts(bstr.as_ptr(), length) };

                String::from_utf16(units)
                    .map_err(|_| XllError::input("RTD topic", InputError::InvalidUtf16))?
            }
            VT_VARIANT | VT_EMPTY => {
                let mut value = VARIANT::default();

                // SAFETY: `index` is within the validated SAFEARRAY bounds and
                // `value` points to initialized writable VARIANT storage. On
                // success VariantGuard clears the copied value exactly once.
                if unsafe {
                    SafeArrayGetElement(array_ptr, &index, (&mut value as *mut VARIANT).cast())
                } < 0
                {
                    return Err(XllError::InvalidHandle);
                }

                let _value = VariantGuard(NonNull::from(&mut value));

                // SAFETY: SafeArrayGetElement successfully initialized `value`,
                // so its VARIANT header and discriminated payload may be read.
                let variant = unsafe { value.Anonymous.Anonymous };

                if variant.vt != VT_BSTR {
                    return Err(XllError::InvalidHandle);
                }

                // SAFETY: the VARIANT discriminant was checked to be VT_BSTR,
                // so `bstrVal` is the active union member.
                let Some(bstr) = NonNull::new(unsafe { variant.Anonymous.bstrVal as *mut u16 })
                else {
                    return Err(XllError::InvalidHandle);
                };

                // SAFETY: `bstr` is owned by the live VARIANT and remains valid
                // until `_value` clears that VARIANT.
                let length = unsafe { SysStringLen(bstr.as_ptr()) } as usize;
                checked_topic_part_length(length)?;

                // SAFETY: the BSTR contains at least `length` UTF-16 code units
                // and `_value` keeps the owning VARIANT alive.
                let units = unsafe { std::slice::from_raw_parts(bstr.as_ptr(), length) };

                String::from_utf16(units)
                    .map_err(|_| XllError::input("RTD topic", InputError::InvalidUtf16))?
            }
            _ => return Err(XllError::InvalidHandle),
        };

        parts.push(part_str);
    }

    parts.pop().ok_or(XllError::InvalidHandle)
}

struct BstrGuard(NonNull<u16>);

impl Drop for BstrGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null BSTR allocated or copied for this
        // guard and has not previously been freed.
        unsafe { SysFreeString(self.0.as_ptr()) };
    }
}

struct VariantGuard(NonNull<VARIANT>);

impl Drop for VariantGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` points to an initialized VARIANT exclusively owned by
        // the surrounding stack frame and not previously cleared.
        let _ = unsafe { VariantClear(self.0.as_ptr()) };
    }
}

fn module_path(callbacks: &HostCallbackSession) -> XllResult<String> {
    // SAFETY: xlGetName takes no arguments. ExcelCallbackValue assumes ownership
    // of the callback result and exposes it through its managed result wrapper.
    let (status, mut result) = unsafe {
        callbacks
            .call(XL_GET_NAME, &[])
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlGetName(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlGetName",
            code: status.raw_code(),
        }));
    }

    let path = String::from_excel(
        result.borrow()?,
        "module",
        &crate::CallContext::without_runtime(),
    )?;
    result.try_release()?;

    Ok(path)
}

struct CountedString {
    units: Box<[u16]>,
    oper: XLOPER12,
}

impl CountedString {
    fn new(value: &str) -> XllResult<Self> {
        let units =
            crate::utf16::encode_counted(value, "RTD topic", crate::utf16::EXCEL_STRING_LIMIT)?;
        let mut units = units.into_boxed_slice();
        let oper = XLOPER12 {
            value: XLOPER12Value {
                string: units.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        Ok(Self { units, oper })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _keep_alive = &self.units;
        NonNull::from(&mut self.oper)
    }
}

struct TemporaryRegistration {
    prog_key: Vec<u16>,
    class_key: Vec<u16>,
    _maintenance: parking_lot::MutexGuard<'static, ()>,
    _cross_process: CrossProcessRegistrationGuard,
}

impl TemporaryRegistration {
    fn new(active: &ActiveServer, module_path: &str) -> XllResult<Self> {
        let maintenance = REGISTRATION_MAINTENANCE.lock();
        let cross_process = CrossProcessRegistrationGuard::acquire()?;

        if let Err(error) = scavenge_owned_registrations(module_path, Some(&active.prog_id)) {
            crate::diagnostics::report_no_unwind("RTD registration scavenging", &error);
        }

        let class = guid_braced(active.class_id);
        let prog_key = format!("Software\\Classes\\{}", active.prog_id);
        let class_key = format!("Software\\Classes\\CLSID\\{class}");
        let registration = Self {
            prog_key: wide_nul(&prog_key),
            class_key: wide_nul(&class_key),
            _maintenance: maintenance,
            _cross_process: cross_process,
        };

        let result = (|| {
            set_registry_value(&prog_key, Some("XlFnOwner"), RTD_REGISTRATION_OWNER)?;
            set_registry_value(
                &prog_key,
                Some("XlFnRegistrationSchema"),
                RTD_REGISTRATION_SCHEMA,
            )?;
            set_registry_value(&prog_key, Some("XlFnOwnerModule"), module_path)?;
            set_registry_value(&prog_key, Some("XlFnClassId"), &class)?;
            set_registry_value(&format!("{prog_key}\\CLSID"), None, &class)?;
            set_registry_value(&format!("{class_key}\\InProcServer32"), None, module_path)?;
            set_registry_value(
                &format!("{class_key}\\InProcServer32"),
                Some("ThreadingModel"),
                "Both",
            )?;
            set_registry_value(&format!("{class_key}\\ProgID"), None, &active.prog_id)?;
            Ok(())
        })();

        if let Err(error) = result {
            drop(registration);
            return Err(error);
        }

        Ok(registration)
    }
}

struct CrossProcessRegistrationGuard {
    handle: HANDLE,
}

impl CrossProcessRegistrationGuard {
    fn acquire() -> XllResult<Self> {
        // The registry location is shared by all xlfn modules for one user, so
        // use one session-wide acquisition order rather than deriving a name
        // from a path string that may have aliases (case, short names, or symlinks).
        Self::acquire_named("Local\\XlFnRtdRegistration_v1")
    }

    fn acquire_named(name: &str) -> XllResult<Self> {
        let name = wide_nul(name);

        // SAFETY: the security attributes are optional, initial ownership is
        // false, and `name` is a live NUL-terminated UTF-16 buffer.
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            // SAFETY: GetLastError has no preconditions and is read immediately
            // after the failed CreateMutexW call.
            let code = (unsafe { GetLastError() }) as i32;
            return Err(XllError::ExcelApi {
                function: "CreateMutexW",
                code,
            });
        }

        // SAFETY: `handle` is a live mutex handle returned by CreateMutexW.
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if !matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            // SAFETY: `handle` is owned by this function and has not been closed.
            unsafe { CloseHandle(handle) };
            return Err(XllError::ExcelApi {
                function: "WaitForSingleObject(RTD registration mutex)",
                code: if wait == WAIT_FAILED {
                    // SAFETY: GetLastError is read immediately after WAIT_FAILED.
                    (unsafe { GetLastError() }) as i32
                } else {
                    wait as i32
                },
            });
        }

        Ok(Self { handle })
    }
}

impl Drop for CrossProcessRegistrationGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns the mutex after a successful wait and closes
        // the handle exactly once after releasing ownership.
        unsafe {
            ReleaseMutex(self.handle);
            CloseHandle(self.handle);
        }
    }
}

fn scavenge_owned_registrations(module_path: &str, keep_prog_id: Option<&str>) -> XllResult<()> {
    for prog_id in rtd_prog_ids()? {
        if keep_prog_id.is_some_and(|keep| prog_id.eq_ignore_ascii_case(keep)) {
            continue;
        }

        let prog_key = format!("Software\\Classes\\{prog_id}");
        let owner = read_registry_string(&prog_key, "XlFnOwner")?;
        let schema = read_registry_string(&prog_key, "XlFnRegistrationSchema")?;
        let owner_module = read_registry_string(&prog_key, "XlFnOwnerModule")?;
        let class_id = read_registry_string(&prog_key, "XlFnClassId")?;

        if owner.as_deref() != Some(RTD_REGISTRATION_OWNER)
            || schema.as_deref() != Some(RTD_REGISTRATION_SCHEMA)
            || !owner_module
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(module_path))
        {
            continue;
        }

        let Some(class_id) = class_id.filter(|value| is_braced_guid(value)) else {
            continue;
        };

        // Delete the CLSID first. If it fails, the marked ProgID remains so a
        // later startup can retry instead of orphaning an undiscoverable CLSID.
        delete_registry_tree(&format!("Software\\Classes\\CLSID\\{class_id}"))?;
        delete_registry_tree(&prog_key)?;
    }

    Ok(())
}

fn delete_registry_tree(path: &str) -> XllResult<()> {
    let path = wide_nul(path);

    // SAFETY: `path` is a valid NUL-terminated HKCU-relative registry subkey.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };

    if matches!(status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(registry_error("RegDeleteTreeW", status))
    }
}

fn rtd_prog_ids() -> XllResult<Vec<String>> {
    let classes_path = wide_nul("Software\\Classes");
    let mut classes: HKEY = ptr::null_mut();

    // SAFETY: `classes_path` is NUL-terminated and `classes` points to writable
    // HKEY output storage.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            classes_path.as_ptr(),
            0,
            KEY_READ,
            &mut classes,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegOpenKeyExW", status));
    }

    let mut names = Vec::new();
    let mut index = 0;

    loop {
        // Windows registry key names are limited to 255 UTF-16 code units.
        let mut name = [0_u16; 256];
        let mut length = name.len() as u32;

        // SAFETY: `classes` is an open registry key, `name` is writable for
        // `length` UTF-16 units, and all unused optional outputs are null.
        let status = unsafe {
            RegEnumKeyExW(
                classes,
                index,
                name.as_mut_ptr(),
                &mut length,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };

        if status == ERROR_NO_MORE_ITEMS {
            break;
        }

        if status != ERROR_SUCCESS {
            // SAFETY: `classes` was successfully returned by RegOpenKeyExW and
            // has not previously been closed.
            unsafe { RegCloseKey(classes) };
            return Err(registry_error("RegEnumKeyExW", status));
        }

        let value = String::from_utf16_lossy(&name[..length as usize]);
        if value.starts_with(RTD_PROG_ID_PREFIX) {
            names.push(value);
        }

        index += 1;
    }

    // SAFETY: `classes` was successfully returned by RegOpenKeyExW and is
    // closed exactly once after enumeration completes.
    unsafe { RegCloseKey(classes) };

    Ok(names)
}

fn read_registry_string(path: &str, name: &str) -> XllResult<Option<String>> {
    let path = wide_nul(path);
    let name = wide_nul(name);
    let mut key: HKEY = ptr::null_mut();

    // SAFETY: `path` is NUL-terminated and `key` points to writable HKEY output
    // storage.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegOpenKeyExW", status));
    }

    let mut value_type = 0;
    let mut bytes = 0;

    // SAFETY: `key` is open, `name` is NUL-terminated, and both metadata
    // outputs are writable. A null data pointer requests the required size.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut value_type,
            ptr::null_mut(),
            &mut bytes,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        // SAFETY: `key` was returned by RegOpenKeyExW and has not yet been closed.
        unsafe { RegCloseKey(key) };
        return Ok(None);
    }

    if status != ERROR_SUCCESS || value_type != REG_SZ {
        // SAFETY: `key` was returned by RegOpenKeyExW and has not yet been closed.
        unsafe { RegCloseKey(key) };

        return if status == ERROR_SUCCESS {
            Ok(None)
        } else {
            Err(registry_error("RegQueryValueExW", status))
        };
    }

    let mut value = vec![0_u16; (bytes as usize).div_ceil(2)];

    // SAFETY: `value` provides at least `bytes` writable bytes, `key` remains
    // open, and all metadata pointers are valid.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut value_type,
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };

    // SAFETY: `key` was returned by RegOpenKeyExW and is closed exactly once.
    unsafe { RegCloseKey(key) };

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegQueryValueExW", status));
    }

    let length = (bytes as usize / 2).min(value.len());
    value.truncate(length);

    while value.last() == Some(&0) {
        value.pop();
    }

    Ok(Some(String::from_utf16_lossy(&value)))
}

fn registry_error(function: &'static str, status: u32) -> XllError {
    XllError::ExcelApi {
        function,
        code: status as i32,
    }
}

fn is_braced_guid(value: &str) -> bool {
    value.len() == 38
        && value.as_bytes()[0] == b'{'
        && value.as_bytes()[37] == b'}'
        && [9, 14, 19, 24]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 0 | 9 | 14 | 19 | 24 | 37) || byte.is_ascii_hexdigit()
        })
}

impl TemporaryRegistration {
    fn close_internal(&mut self) -> XllResult<()> {
        let mut first_error = None;
        // SAFETY: both paths are valid NUL-terminated HKCU subkeys created and
        // owned by this guard.
        unsafe {
            let class_status = RegDeleteTreeW(HKEY_CURRENT_USER, self.class_key.as_ptr());
            if !matches!(class_status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
                first_error = Some(registry_error("RegDeleteTreeW (CLSID)", class_status));
                record_registry_key_debt(String::from_utf16_lossy(&self.class_key));
            }

            let prog_status = RegDeleteTreeW(HKEY_CURRENT_USER, self.prog_key.as_ptr());
            if !matches!(prog_status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
                if first_error.is_none() {
                    first_error = Some(registry_error("RegDeleteTreeW (ProgID)", prog_status));
                }
                record_registry_key_debt(String::from_utf16_lossy(&self.prog_key));
            }
        }

        if let Some(err) = first_error {
            Err(err)
        } else {
            Ok(())
        }
    }
}

static REGISTRY_KEY_DEBT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record_registry_key_debt(key_path: String) {
    let mut debt = REGISTRY_KEY_DEBT.lock();
    if !debt.contains(&key_path) {
        debt.push(key_path);
    }
}

impl Drop for TemporaryRegistration {
    fn drop(&mut self) {
        let _ = self.close_internal();
    }
}

fn set_registry_value(path: &str, name: Option<&str>, value: &str) -> XllResult<()> {
    let path = wide_nul(path);
    let name = name.map(wide_nul);
    let value = wide_nul(value);
    let mut key: HKEY = ptr::null_mut();
    let mut disposition = 0;

    // SAFETY: all input string buffers are NUL-terminated and remain readable.
    // `key` and `disposition` point to writable output storage.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            ptr::null(),
            &mut key,
            &mut disposition,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(XllError::ExcelApi {
            function: "RegCreateKeyExW",
            code: status as i32,
        });
    }

    let bytes = u32::try_from(value.len() * 2).map_err(|_| XllError::Domain {
        code: crate::DomainErrorCode::Overflow,
    })?;

    // SAFETY: `key` is open, the optional name and value buffers are
    // NUL-terminated and readable, and `bytes` does not exceed the value buffer.
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            bytes,
        )
    };

    // SAFETY: `key` was returned by RegCreateKeyExW and is closed exactly once.
    unsafe { RegCloseKey(key) };

    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(XllError::ExcelApi {
            function: "RegSetValueExW",
            code: status as i32,
        })
    }
}

fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn guid_compact(guid: GUID) -> String {
    guid_braced(guid).replace(['{', '}', '-'], "")
}

fn guid_braced(guid: GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7],
    )
}

fn guid_eq(left: GUID, right: GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::subscription::{RtdSink, RtdSource, RtdSubscription, RtdTopic};
    use std::marker::PhantomData;
    use std::ptr;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use crate::win32::{
        COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, RPC_E_CHANGED_MODE, S_FALSE, S_OK,
        SafeArrayGetDim,
    };
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(ServerOperation<'static>: Send, Sync);
    assert_not_impl_any!(ServerNotificationOperation<'static>: Send, Sync);
    assert_not_impl_any!(ServerTermination<'static>: Send, Sync);

    struct TestComApartment {
        should_uninitialize: bool,

        // COM apartment initialization is thread-affine. Making this guard
        // neither Send nor Sync prevents it from being dropped on another thread.
        _not_send_or_sync: PhantomData<Rc<()>>,
    }

    impl TestComApartment {
        fn enter() -> Self {
            // SAFETY:
            // - `pv_reserved` must be null according to the `CoInitializeEx`
            //   contract.
            // - `COINIT_MULTITHREADED` is a valid apartment initialization flag.
            // - The returned HRESULT is checked below.
            // - A successful call, including `S_FALSE`, is balanced by exactly one
            //   `CoUninitialize` call from `Drop` on the same thread.
            let status = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED as u32) };

            match status {
                S_OK | S_FALSE => Self {
                    should_uninitialize: true,
                    _not_send_or_sync: PhantomData,
                },
                RPC_E_CHANGED_MODE => Self {
                    // The current thread was already initialized using a different
                    // apartment model. This call did not initialize COM and must
                    // therefore not be balanced by `CoUninitialize`.
                    should_uninitialize: false,
                    _not_send_or_sync: PhantomData,
                },
                _ => panic!("CoInitializeEx failed: {status:#010x}"),
            }
        }
    }

    impl Drop for TestComApartment {
        fn drop(&mut self) {
            if self.should_uninitialize {
                // SAFETY:
                // - `should_uninitialize` is true only when `CoInitializeEx`
                //   returned `S_OK` or `S_FALSE`.
                // - Each successful `CoInitializeEx` call must be balanced by one
                //   `CoUninitialize` call.
                // - `_not_send_or_sync` prevents this guard from moving to another
                //   thread, so this runs on the thread that initialized COM.
                // - `Drop` runs at most once, so the call is not duplicated.
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    #[test]
    fn server_operation_barrier_waits_and_rejects_new_com_work() {
        use std::sync::mpsc::{self, TryRecvError};
        use std::time::{Duration, Instant};

        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let closing_barrier = Arc::clone(&barrier);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closing = std::thread::spawn(move || {
            let _apartment = TestComApartment::enter();
            let _termination = closing_barrier.close_and_wait().unwrap().unwrap();
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while barrier.state.lock().phase == ServerPhase::Open {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for RTD COM shutdown"
            );
            std::thread::yield_now();
        }
        assert_eq!(closed_rx.try_recv(), Err(TryRecvError::Empty));
        assert!(barrier.enter().is_none());
        assert!(matches!(
            barrier.close_and_wait(),
            Err(ServerCloseError::Reentrant)
        ));

        drop(operation);
        closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        closing.join().unwrap();
    }

    #[test]
    fn server_operation_barrier_rejects_same_thread_close_without_closing() {
        let barrier = ServerOperationBarrier::default();
        let outer = barrier.enter().unwrap();
        let nested = barrier.enter().unwrap();

        assert!(matches!(
            barrier.close_and_wait(),
            Err(ServerCloseError::Reentrant)
        ));
        assert_eq!(barrier.state.lock().phase, ServerPhase::Open);
        assert!(barrier.enter().is_some());

        drop(nested);
        drop(outer);
        let termination = barrier.close_and_wait().unwrap().unwrap();
        assert!(barrier.enter().is_none());
        assert_eq!(
            barrier.state.lock().phase,
            ServerPhase::Terminating {
                owner: std::thread::current().id(),
                deferred: false,
            }
        );
        drop(termination);
        assert_eq!(barrier.state.lock().phase, ServerPhase::Terminated);
        assert!(barrier.close_and_wait().unwrap().is_none());
    }

    #[test]
    fn terminal_close_rejects_same_thread_notification_without_closing() {
        let barrier = ServerOperationBarrier::default();
        let notification = barrier.enter_notification().unwrap();

        assert!(matches!(
            barrier.close_and_wait(),
            Err(ServerCloseError::Reentrant)
        ));
        assert_eq!(barrier.state.lock().phase, ServerPhase::Open);

        drop(notification);
        let termination = barrier.close_and_wait().unwrap().unwrap();
        drop(termination);
        assert_eq!(barrier.state.lock().phase, ServerPhase::Terminated);
    }

    #[test]
    fn server_termination_defers_cross_thread_notification_without_waiting() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let notification = barrier.enter_notification().unwrap();
        let terminating_barrier = Arc::clone(&barrier);

        std::thread::spawn(move || {
            let request = terminating_barrier.request_termination().unwrap();
            assert!(matches!(request, ServerTerminationRequest::Deferred(_)));
        })
        .join()
        .unwrap();

        assert_eq!(barrier.state.lock().phase, ServerPhase::Open);
        drop(notification);

        let termination = match barrier.request_termination().unwrap() {
            ServerTerminationRequest::Synchronous(termination) => termination,
            _ => panic!("quiescent server termination must stay synchronous"),
        };
        assert!(barrier.enter_notification().is_none());
        assert!(matches!(
            barrier.close_and_wait(),
            Err(ServerCloseError::Reentrant)
        ));
        drop(termination);
        assert!(matches!(
            barrier.request_termination().unwrap(),
            ServerTerminationRequest::Complete
        ));
    }

    #[test]
    fn terminal_close_waits_for_notification_quiescence() {
        use std::sync::mpsc::{self, TryRecvError};
        use std::time::{Duration, Instant};

        let barrier = Arc::new(ServerOperationBarrier::default());
        let notification = barrier.enter_notification().unwrap();
        let closing_barrier = Arc::clone(&barrier);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closing = std::thread::spawn(move || {
            let _apartment = TestComApartment::enter();
            let _termination = closing_barrier.close_and_wait().unwrap().unwrap();
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while barrier.state.lock().phase == ServerPhase::Open {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for terminal RTD close"
            );
            std::thread::yield_now();
        }
        assert_eq!(closed_rx.try_recv(), Err(TryRecvError::Empty));
        assert!(matches!(
            barrier.close_and_wait(),
            Err(ServerCloseError::Reentrant)
        ));

        drop(notification);
        closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        closing.join().unwrap();
        assert_eq!(barrier.state.lock().phase, ServerPhase::Terminated);
    }

    #[test]
    fn secondary_terminal_close_waits_for_termination_completion() {
        use std::sync::mpsc::{self, TryRecvError};
        use std::time::Duration;

        let barrier = Arc::new(ServerOperationBarrier::default());
        let owner_barrier = Arc::clone(&barrier);
        let (owner_ready_tx, owner_ready_rx) = mpsc::sync_channel(1);
        let (release_owner_tx, release_owner_rx) = mpsc::sync_channel(1);
        let owner = std::thread::spawn(move || {
            let termination = match owner_barrier.request_termination().unwrap() {
                ServerTerminationRequest::Synchronous(termination) => termination,
                _ => panic!("quiescent server termination must stay synchronous"),
            };
            owner_ready_tx.send(()).unwrap();
            release_owner_rx.recv().unwrap();
            drop(termination);
        });
        owner_ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let secondary_barrier = Arc::clone(&barrier);
        let (secondary_done_tx, secondary_done_rx) = mpsc::sync_channel(1);
        let (secondary_waiting_tx, secondary_waiting_rx) = mpsc::sync_channel(1);
        let secondary = std::thread::spawn(move || {
            let _apartment = TestComApartment::enter();
            assert!(
                secondary_barrier
                    .close_and_wait_with(|event| {
                        secondary_waiting_tx.send(()).unwrap();
                        event.wait_with_com_pumping()
                    })
                    .unwrap()
                    .is_none()
            );
            secondary_done_tx.send(()).unwrap();
        });

        secondary_waiting_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            secondary_done_rx.try_recv(),
            Err(TryRecvError::Empty),
            "secondary close must not pass quiescence before teardown completes"
        );
        release_owner_tx.send(()).unwrap();
        secondary_done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        secondary.join().unwrap();
        owner.join().unwrap();
        assert_eq!(barrier.state.lock().phase, ServerPhase::Terminated);
    }

    #[test]
    fn terminal_wait_failure_reopens_the_operation_gate() {
        use std::sync::mpsc;
        use std::time::Duration;

        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation_barrier = Arc::clone(&barrier);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let operation = operation_barrier.enter().unwrap();
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(operation);
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let error = match barrier.close_and_wait_with(|_| {
            assert!(
                barrier.state.try_lock().is_some(),
                "COM wait must run without the barrier mutex"
            );
            Err(E_FAIL)
        }) {
            Err(error) => error,
            Ok(_) => panic!("injected COM wait failure must be returned"),
        };
        assert_eq!(error, ServerCloseError::WaitFailed(E_FAIL));
        assert_eq!(barrier.state.lock().phase, ServerPhase::Open);

        let accepted_after_rollback = barrier.enter().unwrap();
        drop(accepted_after_rollback);
        release_tx.send(()).unwrap();
        worker.join().unwrap();

        let termination = barrier.close_and_wait().unwrap().unwrap();
        drop(termination);
        assert_eq!(barrier.state.lock().phase, ServerPhase::Terminated);
    }

    // These tests mutate process-global RTD, COM-module, and ingress state.
    // Serialize them with Runtime/async tests and retain the module lease for
    // the complete test so another lifecycle test cannot open or close the
    // process-global module concurrently.
    struct RtdTestLock;

    struct RtdTestGuard {
        // Fields are dropped in declaration order: release the module lease
        // while the shared Runtime test lock is still held.
        _module_lease: crate::ingress::TestModuleLease,
        _runtime_lock: std::sync::MutexGuard<'static, ()>,
    }

    struct TestBoxedFactory(*mut ClassFactory);

    impl TestBoxedFactory {
        fn as_ptr(&self) -> *mut ClassFactory {
            self.0
        }
    }

    impl Drop for TestBoxedFactory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: this guard uniquely owns the Box allocation created
                // by the COM-module lifetime test.
                unsafe { drop(Box::from_raw(self.0)) };
                self.0 = ptr::null_mut();
            }
        }
    }

    struct TestServerLock(*mut ClassFactory);

    impl Drop for TestServerLock {
        fn drop(&mut self) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: the paired TestBoxedFactory remains alive until after
            // this guard and this releases exactly one successful test lock.
            if unsafe { factory_lock_server(self.0, 0) } != S_OK {
                std::process::abort();
            }
            self.0 = ptr::null_mut();
        }
    }

    struct TestClassFactory(NonNull<ClassFactory>);

    impl TestClassFactory {
        fn as_ptr(&self) -> *mut ClassFactory {
            self.0.as_ptr()
        }

        fn vtable(&self) -> &ClassFactoryVtable {
            // SAFETY: `get_test_class_factory` constructs this wrapper only
            // from a successful COM class-factory result with the static
            // implementation vtable.
            unsafe { &*self.0.as_ref().vtable }
        }
    }

    impl Drop for TestClassFactory {
        fn drop(&mut self) {
            // SAFETY: the wrapper owns exactly the factory reference returned
            // by DllGetClassObject.
            unsafe { factory_release(self.as_ptr()) };
        }
    }

    struct TestUnknownReference(NonNull<c_void>);

    impl TestUnknownReference {
        fn new(pointer: *mut c_void) -> Self {
            Self(NonNull::new(pointer).expect("COM returned a null interface"))
        }

        fn as_ptr(&self) -> *mut c_void {
            self.0.as_ptr()
        }

        fn cast<T>(&self) -> NonNull<T> {
            self.0.cast()
        }

        fn iunknown_vtable(&self) -> &IUnknown_Vtbl {
            // SAFETY: every wrapped value is a live COM interface and the
            // IUnknown-compatible vtable is its first ABI field.
            unsafe { &*(*self.as_ptr().cast::<*const IUnknown_Vtbl>()) }
        }
    }

    impl Drop for TestUnknownReference {
        fn drop(&mut self) {
            // SAFETY: this guard owns exactly one COM interface reference.
            unsafe { release_unknown(self.0) };
        }
    }

    fn close_test_ingress() {
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        if ingress.phase() == crate::ingress::PHASE_CLOSING {
            let _ = ingress.seal_and_drain();
        }
    }

    fn cleanup_test_active_server() {
        let pointer = ACTIVE_SERVER.lock().as_ref().map(|active| active.pointer);
        if let Some(pointer) = pointer {
            discard_unpublished_server(pointer, true);
        }
    }

    fn clear_test_shutdown_ghost() {
        // Runtime/lifecycle tests install a process-global shutdown ghost.
        // An RTD unit test owns a synthetic module epoch and must not append
        // resource events to a previous runtime generation.
        *COM_MODULE_LIFETIME.ghost.lock() = None;
    }

    impl Drop for RtdTestGuard {
        fn drop(&mut self) {
            // Test assertions may unwind before their explicit shutdown path.
            // Remove the process-global server before releasing serialization,
            // otherwise Runtime close can wait forever for RTD quiescence.
            clear_test_shutdown_ghost();
            cleanup_test_active_server();
            close_test_ingress();
            crate::rtd::certify_module_unload();
        }
    }

    impl RtdTestLock {
        fn lock(&self) -> Result<RtdTestGuard, std::convert::Infallible> {
            // Runtime and async tests already use this lock around operations
            // that mutate the same process-global state. Recover poisoning so
            // one genuine RTD assertion failure does not turn every later
            // test into an unrelated PoisonError failure.
            let runtime_lock = crate::runtime::tests::TEST_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let module_lease = crate::ingress::acquire_test_module_lease();

            // The module lease proves that no lifecycle test can install a new
            // ghost concurrently. Clear the completed or abandoned generation
            // before cleanup, because releasing an old server also emits RTD
            // resource events.
            clear_test_shutdown_ghost();

            // COM entry points reject calls unless the global ingress is OPEN.
            // Establish that precondition explicitly rather than depending on
            // another concurrently running lifecycle test.
            cleanup_test_active_server();
            close_test_ingress();
            let ingress = crate::ingress::global_ingress();
            ingress.begin_opening();
            ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
            crate::rtd::begin_module_open();

            Ok(RtdTestGuard {
                _module_lease: module_lease,
                _runtime_lock: runtime_lock,
            })
        }
    }

    static TEST_LOCK: RtdTestLock = RtdTestLock;

    #[test]
    fn com_module_lifetime_tracks_calls_factories_and_server_locks() {
        let _guard = TEST_LOCK.lock().unwrap();
        let ingress = crate::ingress::global_ingress();
        ingress.begin_close_with(|| {});
        let _ = ingress.seal_and_drain();
        crate::rtd::certify_module_unload();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());
        assert_eq!(dll_can_unload_now(), S_OK);

        {
            let _call = COM_MODULE_LIFETIME.enter_call();
            let entered = COM_MODULE_LIFETIME.snapshot();
            assert_eq!(entered.in_flight_calls, baseline.in_flight_calls + 1);
            assert_eq!(dll_can_unload_now(), S_FALSE);
        }
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);

        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        crate::rtd::begin_module_open();

        let factory = TestBoxedFactory(Box::into_raw(Box::new(ClassFactory {
            vtable: &CLASS_FACTORY_VTABLE,
            references: AtomicU32::new(1),
            server: ptr::null_mut(),
            _module_lease: ComObjectLease::new(ComObjectKind::Factory),
        })));
        assert_eq!(
            COM_MODULE_LIFETIME.snapshot().live_factories,
            baseline.live_factories + 1
        );
        assert_eq!(dll_can_unload_now(), S_FALSE);

        let pointer = factory.as_ptr();
        // SAFETY: `pointer` is retained by TestBoxedFactory and LockServer does
        // not inspect the null server field in this lifetime-only test.
        assert_eq!(unsafe { factory_lock_server(pointer, 1) }, S_OK);
        let server_lock = TestServerLock(pointer);
        assert_eq!(
            COM_MODULE_LIFETIME.snapshot().server_locks,
            baseline.server_locks + 1
        );

        ingress.begin_close_with(|| {});
        crate::rtd::begin_module_close();
        assert_eq!(dll_can_unload_now(), S_FALSE);

        // New locks are rejected after close admission stops, while releasing
        // an existing module hold remains available.
        assert_eq!(
            // SAFETY: `pointer` is the live class-factory instance created above. This
            // test intentionally exercises LockServer(TRUE) through the COM ABI.
            unsafe { factory_lock_server(pointer, 1) },
            CO_E_SERVER_STOPPING
        );
        drop(server_lock);
        assert_eq!(
            COM_MODULE_LIFETIME.snapshot().server_locks,
            baseline.server_locks
        );
        assert_eq!(
            // SAFETY: `pointer` still refers to the same live class factory. Unlocking is
            // permitted even after shutdown admission has closed so the module hold can be
            // released during teardown.
            unsafe { factory_lock_server(pointer, 0) },
            E_UNEXPECTED
        );
        drop(factory);

        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
        let _ = ingress.seal_and_drain();
        crate::rtd::certify_module_unload();
        assert_eq!(dll_can_unload_now(), S_OK);

        let server = ComObjectLease::new(ComObjectKind::Server);
        assert_eq!(dll_can_unload_now(), S_FALSE);
        drop(server);
        assert_eq!(dll_can_unload_now(), S_OK);
    }

    #[test]
    fn com_module_lifetime_emits_rtd_resource_trace_events() {
        let _guard = TEST_LOCK.lock().unwrap();
        let ingress = crate::ingress::global_ingress();
        ingress.begin_close_with(|| {});
        let _ = ingress.seal_and_drain();
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        crate::rtd::begin_module_open();

        let ghost = Arc::new(crate::shutdown_refinement::ShutdownGhost::new());
        ghost
            .begin_generation(1, crate::shutdown_refinement::GhostResources::opened(0, 0))
            .unwrap();
        COM_MODULE_LIFETIME.set_ghost(Arc::clone(&ghost));

        let (call, accepted) = COM_MODULE_LIFETIME.enter_call();
        assert!(accepted);
        let factory = ComObjectLease::new(ComObjectKind::Factory);
        let server = ComObjectLease::new(ComObjectKind::Server);
        assert!(COM_MODULE_LIFETIME.set_server_lock(true));
        assert!(COM_MODULE_LIFETIME.set_server_lock(false));
        drop(server);
        drop(factory);
        drop(call);

        let trace = ghost.trace_json().unwrap();
        if let Some(path) = std::env::var_os("XLFN_WINDOWS_RTD_TRACE") {
            std::fs::write(path, &trace).expect("write Windows RTD shutdown trace");
        }
        *COM_MODULE_LIFETIME.ghost.lock() = None;
        ingress.begin_close_with(|| {});
        let _ = ingress.seal_and_drain();
        crate::rtd::certify_module_unload();

        for event in [
            "beginRtdOperation",
            "endRtdOperation",
            "addRtdClassFactory",
            "removeRtdClassFactory",
            "addRtdServer",
            "removeRtdServer",
            "lockRtdServer",
            "unlockRtdServer",
        ] {
            assert!(
                trace.contains(event),
                "RTD trace is missing {event}: {trace}"
            );
        }
    }

    #[test]
    fn registered_git_cookie_blocks_module_unload() {
        let _guard = TEST_LOCK.lock().unwrap();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());

        COM_MODULE_LIFETIME.git_cookie_registered();
        let registered = COM_MODULE_LIFETIME.snapshot();
        assert_eq!(registered.outstanding_git_cookies, 1);
        assert_eq!(registered.revocation_debt, 0);
        assert!(!COM_MODULE_LIFETIME.can_unload_now());

        COM_MODULE_LIFETIME.git_cookie_revoked();
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
    }

    #[test]
    fn git_revocation_retry_in_flight_keeps_unload_blocked() {
        let _guard = TEST_LOCK.lock().unwrap();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());
        let cookie = NonZeroU32::new(41).unwrap();

        COM_MODULE_LIFETIME.git_cookie_registered();
        COM_MODULE_LIFETIME.git_cookie_revocation_deferred(cookie);

        let (claimed_tx, claimed_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let retry = thread::spawn(move || {
            retry_git_revocation_debt_with(|cookie| {
                claimed_tx.send(cookie).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            });
        });

        assert_eq!(claimed_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 41);
        let retrying = COM_MODULE_LIFETIME.snapshot();
        assert_eq!(retrying.revocation_debt, 1);
        assert!(COM_MODULE_LIFETIME.queued_git_revocation_debt().is_empty());
        assert!(!COM_MODULE_LIFETIME.can_unload_now());

        release_tx.send(()).unwrap();
        retry.join().unwrap();
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
    }

    #[test]
    fn panicking_git_revocation_retry_requeues_claim() {
        let _guard = TEST_LOCK.lock().unwrap();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());
        let cookie = NonZeroU32::new(41).unwrap();

        COM_MODULE_LIFETIME.git_cookie_registered();
        COM_MODULE_LIFETIME.git_cookie_revocation_deferred(cookie);

        let result = catch_unwind(AssertUnwindSafe(|| {
            retry_git_revocation_debt_with(|_| panic!("injected GIT revoke panic"));
        }));
        assert!(result.is_err());
        assert_eq!(COM_MODULE_LIFETIME.snapshot().revocation_debt, 1);
        assert_eq!(
            COM_MODULE_LIFETIME.queued_git_revocation_debt(),
            vec![cookie]
        );

        retry_git_revocation_debt_with(|cookie| {
            assert_eq!(cookie, 41);
            Ok(())
        });
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
    }

    #[test]
    fn module_quiescence_refuses_debt_claim_in_flight() {
        let _guard = TEST_LOCK.lock().unwrap();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());
        let cookie = NonZeroU32::new(41).unwrap();

        COM_MODULE_LIFETIME.git_cookie_registered();
        COM_MODULE_LIFETIME.git_cookie_revocation_deferred(cookie);
        let claims = COM_MODULE_LIFETIME.claim_git_revocation_debt_batch();
        assert_eq!(claims.len(), 1);
        assert!(COM_MODULE_LIFETIME.queued_git_revocation_debt().is_empty());

        let error = crate::rtd::wait_for_module_quiescence().unwrap_err();
        assert_eq!(error.outstanding_git_cookies, 0);
        assert_eq!(error.revocation_debt, 1);
        assert!(!COM_MODULE_LIFETIME.can_unload_now());

        drop(claims);
        retry_git_revocation_debt_with(|cookie| {
            assert_eq!(cookie, 41);
            Ok(())
        });
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
    }

    #[test]
    fn server_start_reservation_is_single_use_and_rolls_back_failure() {
        let state = AtomicU8::new(SERVER_NOT_STARTED);

        let first = ServerStartReservation::acquire(&state).unwrap();
        assert_eq!(state.load(Ordering::Acquire), SERVER_STARTING);
        assert!(ServerStartReservation::acquire(&state).is_none());

        drop(first);
        assert_eq!(state.load(Ordering::Acquire), SERVER_NOT_STARTED);

        ServerStartReservation::acquire(&state).unwrap().commit();
        assert_eq!(state.load(Ordering::Acquire), SERVER_STARTED);
        assert!(ServerStartReservation::acquire(&state).is_none());

        let failed_state = AtomicU8::new(SERVER_NOT_STARTED);
        let mut failed = ServerStartReservation::acquire(&failed_state).unwrap();
        failed.callback_published();
        drop(failed);
        assert_eq!(failed_state.load(Ordering::Acquire), SERVER_START_FAILED);
        assert!(ServerStartReservation::acquire(&failed_state).is_none());
    }

    #[test]
    fn server_terminate_reentry_is_deferred_and_idempotent() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        // SAFETY: ACTIVE_SERVER and `ensured` retain the allocation throughout
        // this test, including after deferred ACTIVE cleanup is postponed.
        let server_ref = unsafe { &*server };

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server while this test
        // models a synchronous COM callback from an entered RTD method.
        let operation = server_ref.operations.enter().unwrap();
        // SAFETY: the same retained server is live. ServerTerminate must return
        // immediately and transfer the busy cleanup to its coordinator.
        assert_eq!(unsafe { server_terminate(server) }, S_OK);
        // A duplicate request observes the linearized phase and is idempotent.
        // SAFETY: `server_ref` proves that the raw server remains live.
        assert_eq!(unsafe { server_terminate(server) }, S_OK);
        let phase = server_ref.operations.state.lock().phase;
        assert!(matches!(
            phase,
            ServerPhase::Terminating { deferred: true, .. }
        ));
        drop(operation);

        // The initiating Excel thread is allowed to wait after the original
        // ServerTerminate call has unwound. It pumps COM until the coordinator
        // has signaled the terminal phase, then joins the actual thread.
        assert!(server_ref.operations.close_and_wait().unwrap().is_none());
        server_ref.termination_worker.join().unwrap();
        assert_eq!(
            server_ref.operations.state.lock().phase,
            ServerPhase::Terminated
        );

        shutdown(handles).unwrap();
        drop(ensured);
    }

    #[test]
    fn deferred_termination_drains_callbacks_and_rejects_worker_self_close() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for this test.
        let server_ref = unsafe { &*server };
        let server_address = server as usize;
        let callback_dropped = Arc::new(AtomicBool::new(false));
        let worker_self_close_rejected = Arc::new(AtomicBool::new(false));

        let drop_hook = {
            let callback_dropped = Arc::clone(&callback_dropped);
            let worker_self_close_rejected = Arc::clone(&worker_self_close_rejected);
            Arc::new(move || {
                callback_dropped.store(true, Ordering::Release);
                let server = server_address as *mut RtdServer;
                // SAFETY: the deferred worker reference and ACTIVE_SERVER keep
                // the object live throughout callback revocation.
                let rejected = matches!(
                    unsafe { (*server).operations.close_and_wait() },
                    Err(ServerCloseError::Reentrant)
                );
                worker_self_close_rejected.store(rejected, Ordering::Release);
            })
        };
        let callback = Arc::new(RetainedUpdateCallback {
            cookie: None,
            drop_hook: Some(drop_hook),
        });
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server.
        unsafe { install_callback(&(*server).callbacks, callback) };

        // Model UpdateNotify already in flight when Excel calls
        // ServerTerminate. The COM call returns immediately.
        // SAFETY: the retained server remains live.
        let notification = server_ref.operations.enter_notification().unwrap();
        // SAFETY: `server_ref` proves that the raw server remains live.
        assert_eq!(unsafe { server_terminate(server) }, S_OK);
        assert!(!callback_dropped.load(Ordering::Acquire));
        drop(notification);

        // The initiating Excel thread may now pump until phase completion, then
        // must join the coordinator before any ACTIVE_SERVER removal.
        assert!(server_ref.operations.close_and_wait().unwrap().is_none());
        server_ref.termination_worker.join().unwrap();
        assert!(callback_dropped.load(Ordering::Acquire));
        assert!(worker_self_close_rejected.load(Ordering::Acquire));
        // Deferred cleanup deliberately retains ACTIVE_SERVER until a joiner
        // executes the final reap.
        assert!(
            ACTIVE_SERVER
                .lock()
                .as_ref()
                .is_some_and(|active| active.pointer == server_address)
        );

        shutdown(handles).unwrap();
        drop(ensured);
    }

    #[test]
    fn deferred_termination_spawn_failure_rolls_back_atomically() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for this test.
        let server_ref = unsafe { &*server };
        let operation = server_ref.operations.enter().unwrap();

        FAIL_DEFERRED_TERMINATION_SPAWN.store(true, Ordering::Release);
        // SAFETY: `server_ref` proves that the raw server remains live.
        assert_eq!(unsafe { server_terminate(server) }, E_FAIL);
        assert_eq!(server_ref.operations.state.lock().phase, ServerPhase::Open);
        assert_eq!(
            server_ref.termination_worker.state.lock().status,
            TerminationWorkerStatus::Idle
        );
        // The failed reservation did not leave the operation gate closed.
        let accepted = server_ref.operations.enter().unwrap();
        drop(accepted);
        drop(operation);

        shutdown(handles).unwrap();
        drop(ensured);
    }

    #[test]
    fn termination_worker_can_finish_before_handle_registration() {
        use std::sync::mpsc::{self, TryRecvError};
        use std::time::Duration;

        let worker = Arc::new(TerminationWorker::default());
        let start = worker.reserve_start().unwrap();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || finished_tx.send(()).unwrap());
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Joining callers must wait while the spawner still owns Starting.
        let joining_worker = Arc::clone(&worker);
        let (joined_tx, joined_rx) = mpsc::sync_channel(1);
        let joining = std::thread::spawn(move || {
            joining_worker.join().unwrap();
            joined_tx.send(()).unwrap();
        });
        assert_eq!(joined_rx.try_recv(), Err(TryRecvError::Empty));

        // The OS thread may already have exited; registering its JoinHandle
        // still transitions Starting -> Running without losing ownership.
        start.commit(handle);
        joined_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        joining.join().unwrap();
        assert_eq!(worker.state.lock().status, TerminationWorkerStatus::Joined);
    }

    #[test]
    fn deferred_cleanup_panic_signals_phase_and_is_detected_by_join() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for this test.
        let server_ref = unsafe { &*server };
        let operation = server_ref.operations.enter().unwrap();

        PANIC_DEFERRED_TERMINATION_CLEANUP.store(true, Ordering::Release);
        // SAFETY: `server_ref` proves that the raw server remains live.
        assert_eq!(unsafe { server_terminate(server) }, S_OK);
        drop(operation);
        assert!(server_ref.operations.close_and_wait().unwrap().is_none());
        assert!(matches!(
            server_ref.termination_worker.join(),
            Err(ServerCloseError::WorkerPanicked)
        ));
        assert_eq!(
            server_ref.operations.state.lock().phase,
            ServerPhase::Terminated
        );

        // The coordinator has exited, so ordinary shutdown can safely perform
        // the idempotent cleanup that the injected panic skipped.
        shutdown(handles).unwrap();
        drop(ensured);
    }

    #[test]
    fn failed_git_revocation_is_retained_and_retryable() {
        let _guard = TEST_LOCK.lock().unwrap();
        let baseline = COM_MODULE_LIFETIME.snapshot();
        assert!(baseline.is_quiescent());
        assert!(COM_MODULE_LIFETIME.queued_git_revocation_debt().is_empty());

        let cookie = NonZeroU32::new(41).unwrap();
        COM_MODULE_LIFETIME.git_cookie_registered();
        COM_MODULE_LIFETIME.git_cookie_revocation_deferred(cookie);
        let error = XllError::ExcelApi {
            function: "IGlobalInterfaceTable::RevokeInterfaceFromGlobal",
            code: E_FAIL,
        };
        assert_eq!(COM_MODULE_LIFETIME.snapshot().outstanding_git_cookies, 0);
        assert_eq!(COM_MODULE_LIFETIME.snapshot().revocation_debt, 1);
        assert_eq!(
            COM_MODULE_LIFETIME.queued_git_revocation_debt(),
            vec![cookie]
        );

        let mut attempts = 0;
        retry_git_revocation_debt_with(|cookie| {
            attempts += 1;
            assert_eq!(cookie, 41);
            Err(error.clone())
        });
        assert_eq!(attempts, 1);
        assert_eq!(COM_MODULE_LIFETIME.snapshot().revocation_debt, 1);
        assert_eq!(
            COM_MODULE_LIFETIME.queued_git_revocation_debt(),
            vec![NonZeroU32::new(41).unwrap()]
        );

        retry_git_revocation_debt_with(|cookie| {
            attempts += 1;
            assert_eq!(cookie, 41);
            Ok(())
        });
        assert_eq!(attempts, 2);
        assert_eq!(COM_MODULE_LIFETIME.snapshot(), baseline);
        assert!(COM_MODULE_LIFETIME.queued_git_revocation_debt().is_empty());
    }

    #[test]
    fn retired_callback_drop_can_reenter_terminate_after_quiescence() {
        use std::sync::atomic::AtomicI32;

        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        let server_address = server as usize;

        let dropped_while_active = Arc::new(AtomicBool::new(false));
        let callback_lock_was_free = Arc::new(AtomicBool::new(false));
        let reentrant_status = Arc::new(AtomicI32::new(i32::MIN));

        let drop_hook = {
            let dropped_while_active = Arc::clone(&dropped_while_active);
            let callback_lock_was_free = Arc::clone(&callback_lock_was_free);
            let reentrant_status = Arc::clone(&reentrant_status);
            Arc::new(move || {
                let server_ptr = server_address as *mut RtdServer;
                // SAFETY: `ensured` retains the server until after the outer
                // termination and this hook have both returned.
                let server = unsafe { &*server_ptr };
                let in_flight = server.operations.state.lock().in_flight;
                if in_flight != 0 {
                    dropped_while_active.store(true, Ordering::Release);
                    return;
                }

                // Avoid hanging the test if a future regression drops while
                // holding the callback mutex; record the violation instead.
                let lock_was_free = server.callbacks.try_lock().is_some();
                callback_lock_was_free.store(lock_was_free, Ordering::Release);
                if !lock_was_free {
                    return;
                }

                // SAFETY: the server is retained and quiescent. This models a
                // GIT revoke synchronously releasing COM code that re-enters
                // the same server's idempotent ServerTerminate method.
                reentrant_status.store(unsafe { server_terminate(server_ptr) }, Ordering::Release);
            })
        };

        let previous = Arc::new(RetainedUpdateCallback {
            cookie: None,
            drop_hook: Some(drop_hook),
        });
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server.
        unsafe { install_callback(&(*server).callbacks, previous) };

        // Model replacement during ServerStart. The previous callback must be
        // retained rather than released while this operation is in flight.
        // SAFETY: the retained server remains live.
        let operation = unsafe { (*server).operations.enter() }.unwrap();
        let replacement = Arc::new(RetainedUpdateCallback {
            cookie: None,
            drop_hook: None,
        });
        // SAFETY: the retained server remains live.
        unsafe { install_callback(&(*server).callbacks, replacement) };
        assert!(!dropped_while_active.load(Ordering::Acquire));
        assert_eq!(reentrant_status.load(Ordering::Acquire), i32::MIN);
        drop(operation);

        // SAFETY: the retained server is now quiescent. The callback hook
        // performs one nested ServerTerminate while the outer call drains it.
        assert_eq!(unsafe { server_terminate(server) }, S_OK);
        assert!(!dropped_while_active.load(Ordering::Acquire));
        assert!(callback_lock_was_free.load(Ordering::Acquire));
        assert_eq!(reentrant_status.load(Ordering::Acquire), S_OK);

        drop(ensured);
        handles.close().unwrap();
    }

    #[test]
    fn callback_subscription_attach_handshake_covers_early_empty_snapshot() {
        use std::sync::Barrier;

        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;
        let _generation = ensured.active.generation;

        // Model ServerStart's early backend snapshot before another thread
        // attaches subscriptions.
        // SAFETY: ACTIVE_SERVER and `ensured` retain the server.
        assert!(unsafe { (*server).backends.lock().subscriptions.is_none() });
        // SAFETY: the retained server remains live for the scoped race.
        let operation = unsafe { (*server).operations.enter() }.unwrap();

        let subscriptions = Arc::new(SubscriptionRuntime::new());
        let rendezvous = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let attached_subscriptions = Arc::clone(&subscriptions);
            let attached_rendezvous = Arc::clone(&rendezvous);
            scope.spawn(move || {
                let attached = ensure_server(None, Some(attached_subscriptions))
                    .expect("attach subscriptions");
                attached_rendezvous.wait();
                attached_rendezvous.wait();
                drop(attached);
            });

            // The attaching side has published subscriptions and observed that
            // no callback exists yet.
            rendezvous.wait();

            let callback = Arc::new(RetainedUpdateCallback {
                cookie: None,
                drop_hook: None,
            });
            // SAFETY: the retained server remains live.
            unsafe { install_callback(&(*server).callbacks, Arc::clone(&callback)) };
            // SAFETY: the same retained server remains live. This post-install
            // re-read must observe the attachment made before the barrier.
            unsafe { synchronize_callback_notification(&*server, Arc::clone(&callback)) }.unwrap();

            // local + server active slot + SubscriptionRuntime notification.
            assert_eq!(Arc::strong_count(&callback), 3);
            rendezvous.wait();
        });

        drop(operation);
        shutdown_subscriptions(subscriptions).unwrap();
        drop(ensured);
        handles.close().unwrap();
    }

    fn iid_null_from_fields() -> GUID {
        GUID {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        }
    }

    fn iid_iunknown_from_fields() -> GUID {
        GUID {
            data1: 0x0000_0000,
            data2: 0x0000,
            data3: 0x0000,
            data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
        }
    }

    fn iid_iclass_factory_from_fields() -> GUID {
        GUID {
            data1: 0x0000_0001,
            data2: 0x0000,
            data3: 0x0000,
            data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
        }
    }

    fn iid_idispatch_from_fields() -> GUID {
        GUID {
            data1: 0x0002_0400,
            data2: 0x0000,
            data3: 0x0000,
            data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
        }
    }

    fn iid_irtd_server_from_fields() -> GUID {
        GUID {
            data1: 0xec0e_6191,
            data2: 0xdb51,
            data3: 0x11d3,
            data4: [0x8f, 0x3e, 0x00, 0xc0, 0x4f, 0x36, 0x51, 0xb8],
        }
    }

    unsafe fn release_unknown(interface: NonNull<c_void>) -> u32 {
        // SAFETY: callers pass one owned reference to a live COM interface. All
        // COM interfaces begin with an IUnknown-compatible vtable.
        let vtable = unsafe { *interface.as_ptr().cast::<*const IUnknown_Vtbl>() };
        // SAFETY: `vtable` came from the same live interface and `interface`
        // owns exactly one reference for this release.
        unsafe { ((*vtable).Release)(interface.as_ptr()) }
    }

    fn get_test_class_factory(active: &ActiveServer) -> TestClassFactory {
        let iid = iid_iclass_factory_from_fields();
        let mut output = ptr::null_mut();

        // SAFETY: all GUIDs and the output slot remain live for the call. The
        // IID is independently field-constructed rather than copied from the
        // implementation constant.
        let status = unsafe {
            dll_get_class_object(
                (&active.class_id as *const GUID).cast(),
                (&iid as *const GUID).cast(),
                &mut output,
            )
        };
        assert_eq!(status, S_OK);
        TestClassFactory(
            NonNull::new(output.cast()).expect("DllGetClassObject returned null factory"),
        )
    }

    struct DispatchTestSubscription {
        disconnected: Arc<AtomicBool>,
    }

    // SAFETY: DispatchTestSubscription is a mock subscription for testing.
    unsafe impl RtdSubscription for DispatchTestSubscription {
        fn request_cancel(&self) {}

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.disconnected.store(true, Ordering::Release);
            Ok(())
        }
    }

    struct DispatchTestSource {
        sink: Mutex<Option<RtdSink<f64>>>,
        disconnected: Arc<AtomicBool>,
    }

    impl DispatchTestSource {
        fn publish(&self, value: f64) -> XllResult<()> {
            self.sink
                .lock()
                .as_ref()
                .ok_or(XllError::Internal {
                    diagnostic_id: 0x5254_4444_4953_5054,
                })?
                .publish(value)
        }
    }

    impl RtdSource for DispatchTestSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            sink.publish(12.5)?;
            self.sink.lock().replace(sink);
            Ok(Box::new(DispatchTestSubscription {
                disconnected: Arc::clone(&self.disconnected),
            }))
        }
    }

    #[test]
    fn com_boundary_converts_panics_to_e_unexpected() {
        let _guard = TEST_LOCK.lock().unwrap();
        assert_eq!(com_boundary("test COM boundary", || S_OK), S_OK);
        assert_eq!(
            com_boundary("test COM boundary", || panic!("injected COM panic")),
            E_UNEXPECTED
        );
    }

    #[test]
    fn refresh_data_converts_panics_to_e_unexpected() {
        let _guard = TEST_LOCK.lock().unwrap();
        PANIC_IN_REFRESH_DATA.store(true, Ordering::Release);
        let mut topic_count = 0;
        let mut result = ptr::null_mut();

        assert_eq!(
            // SAFETY: the injected panic occurs before RefreshData reads any of the
            // supplied COM pointers.
            unsafe { refresh_data(ptr::null_mut(), &mut topic_count, &mut result) },
            E_UNEXPECTED
        );
    }

    #[test]
    fn standard_com_iids_match_their_field_definitions() {
        assert!(guid_eq(IID_NULL, iid_null_from_fields()));
        assert!(guid_eq(IID_IUNKNOWN, iid_iunknown_from_fields()));
        assert!(guid_eq(
            IID_ICLASS_FACTORY,
            iid_iclass_factory_from_fields()
        ));
        assert!(guid_eq(IID_IDISPATCH, iid_idispatch_from_fields()));
        assert!(guid_eq(IID_IRTD_SERVER, iid_irtd_server_from_fields()));
        assert!(guid_eq(
            IID_IRTD_UPDATE_EVENT,
            GUID {
                data1: 0xa437_88c1,
                data2: 0xd91b,
                data3: 0x11d3,
                data4: [0x8f, 0x39, 0x00, 0xc0, 0x4f, 0x36, 0x51, 0xb8],
            }
        ));
    }

    #[test]
    fn iunknown_vtable_has_three_pointer_slots() {
        assert_eq!(
            std::mem::size_of::<IUnknown_Vtbl>(),
            3 * std::mem::size_of::<usize>(),
        );
        assert_eq!(
            std::mem::align_of::<IUnknown_Vtbl>(),
            std::mem::align_of::<usize>(),
        );
    }

    #[test]
    fn refresh_data_arrays_have_two_rows_for_small_and_large_batches() {
        let _apartment = TestComApartment::enter();
        for count in [1, 2, 3, 100] {
            let updates = (0..count)
                .map(|column| {
                    RtdUpdate::for_test(100 + column, RtdValue::Number(f64::from(200 + column)))
                })
                .collect::<Vec<_>>();
            let mut topic_count = -1;
            let mut array = ptr::null_mut();

            assert_eq!(
                // SAFETY: both outputs are writable, `updates` remains readable, and
                // the returned SAFEARRAY is inspected and destroyed exactly once.
                unsafe { write_refresh_data(&mut topic_count, &mut array, &updates) },
                S_OK
            );

            assert_eq!(topic_count, count);
            assert!(!array.is_null());

            // SAFETY: write_refresh_data returned a live SAFEARRAY owned by this
            // test and it has not yet been destroyed.
            assert_eq!(unsafe { SafeArrayGetDim(array) }, 2);

            let mut first_lower = -1;
            let mut first_upper = -1;
            let mut second_lower = -1;
            let mut second_upper = -1;

            // SAFETY: `array` is a live two-dimensional SAFEARRAY and all bound
            // output pointers are writable.
            unsafe {
                assert_eq!(SafeArrayGetLBound(array, 1, &mut first_lower), S_OK);
                assert_eq!(SafeArrayGetUBound(array, 1, &mut first_upper), S_OK);
                assert_eq!(SafeArrayGetLBound(array, 2, &mut second_lower), S_OK);
                assert_eq!(SafeArrayGetUBound(array, 2, &mut second_upper), S_OK);
            }

            assert_eq!((first_lower, first_upper), (0, 1));
            assert_eq!((second_lower, second_upper), (0, count - 1));

            for column in 0..count {
                let mut topic = VARIANT::default();
                let mut value = VARIANT::default();
                let mut topic_index = [0, column];
                let mut value_index = [1, column];

                // SAFETY: both indices are within the validated array bounds and
                // both VARIANT outputs are initialized writable storage.
                unsafe {
                    assert_eq!(
                        SafeArrayGetElement(
                            array,
                            topic_index.as_mut_ptr(),
                            (&mut topic as *mut VARIANT).cast(),
                        ),
                        S_OK
                    );
                    assert_eq!(
                        SafeArrayGetElement(
                            array,
                            value_index.as_mut_ptr(),
                            (&mut value as *mut VARIANT).cast(),
                        ),
                        S_OK
                    );
                }

                // SAFETY: SafeArrayGetElement successfully initialized both
                // VARIANTs. The checked discriminants select the union fields
                // read below, and both values are cleared exactly once.
                unsafe {
                    assert_eq!(topic.Anonymous.Anonymous.vt, VT_I4);
                    assert_eq!(value.Anonymous.Anonymous.vt, VT_R8);
                    assert_eq!(topic.Anonymous.Anonymous.Anonymous.lVal, 100 + column);
                    assert_eq!(
                        value.Anonymous.Anonymous.Anonymous.dblVal,
                        f64::from(200 + column)
                    );
                    VariantClear(&mut topic);
                    VariantClear(&mut value);
                }
            }

            // SAFETY: ownership was not transferred from this test and the live
            // SAFEARRAY is destroyed exactly once.
            assert_eq!(unsafe { SafeArrayDestroy(array) }, S_OK);
        }
    }

    #[test]
    fn refresh_data_preserves_every_rtd_scalar_variant_by_column_and_row() {
        let _apartment = TestComApartment::enter();
        let updates = [
            RtdUpdate::for_test(201, RtdValue::Number(12.5)),
            RtdUpdate::for_test(202, RtdValue::Integer(-17)),
            RtdUpdate::for_test(203, RtdValue::Boolean(true)),
            RtdUpdate::for_test(204, RtdValue::String("stream value".to_owned())),
            RtdUpdate::for_test(
                205,
                RtdValue::Error(crate::ExcelErrorValue(crate::ExcelError::NotAvailable)),
            ),
            RtdUpdate::for_test(206, RtdValue::Empty),
        ];
        let mut topic_count = -1;
        let mut array = ptr::null_mut();

        assert_eq!(
            // SAFETY: both outputs are writable, `updates` remains readable, and
            // the returned SAFEARRAY is inspected and destroyed exactly once.
            unsafe { write_refresh_data(&mut topic_count, &mut array, &updates) },
            S_OK
        );
        assert_eq!(topic_count, i32::try_from(updates.len()).unwrap());

        for (column, update) in updates.iter().enumerate() {
            let column = i32::try_from(column).unwrap();
            let mut topic = VARIANT::default();
            let mut value = VARIANT::default();
            let mut topic_index = [0, column];
            let mut value_index = [1, column];

            // SAFETY: the logical RTD table has `updates.len()` columns and two
            // rows. Automation receives those indices in [column, row] order,
            // and both VARIANT outputs are writable.
            unsafe {
                assert_eq!(
                    SafeArrayGetElement(
                        array,
                        topic_index.as_mut_ptr(),
                        (&mut topic as *mut VARIANT).cast(),
                    ),
                    S_OK
                );
                assert_eq!(
                    SafeArrayGetElement(
                        array,
                        value_index.as_mut_ptr(),
                        (&mut value as *mut VARIANT).cast(),
                    ),
                    S_OK
                );
                assert_eq!(topic.Anonymous.Anonymous.vt, VT_I4);
                assert_eq!(topic.Anonymous.Anonymous.Anonymous.lVal, update.topic_id);

                match &update.value {
                    RtdValue::Number(expected) => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_R8);
                        assert_eq!(value.Anonymous.Anonymous.Anonymous.dblVal, *expected);
                    }
                    RtdValue::Integer(expected) => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_I4);
                        assert_eq!(value.Anonymous.Anonymous.Anonymous.lVal, *expected);
                    }
                    RtdValue::Boolean(expected) => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_BOOL);
                        assert_eq!(
                            value.Anonymous.Anonymous.Anonymous.boolVal,
                            if *expected {
                                VARIANT_TRUE
                            } else {
                                VARIANT_FALSE
                            }
                        );
                    }
                    RtdValue::String(expected) => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_BSTR);
                        let bstr = value.Anonymous.Anonymous.Anonymous.bstrVal;
                        assert!(!bstr.is_null());
                        let length = SysStringLen(bstr) as usize;
                        let actual =
                            String::from_utf16_lossy(std::slice::from_raw_parts(bstr, length));
                        assert_eq!(actual, *expected);
                    }
                    RtdValue::Error(expected) => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_ERROR);
                        assert_eq!(
                            value.Anonymous.Anonymous.Anonymous.scode,
                            2000 + expected.0.code()
                        );
                    }
                    RtdValue::Empty => {
                        assert_eq!(value.Anonymous.Anonymous.vt, VT_EMPTY);
                    }
                }

                VariantClear(&mut topic);
                VariantClear(&mut value);
            }
        }

        // SAFETY: ownership was not transferred from this test and the live
        // SAFEARRAY is destroyed exactly once.
        assert_eq!(unsafe { SafeArrayDestroy(array) }, S_OK);
    }

    #[test]
    fn topic_key_limits_reject_extreme_bounds_and_oversized_strings() {
        assert_eq!(
            checked_topic_part_count(0, 252).unwrap(),
            MAX_RTD_TOPIC_PARTS
        );
        assert!(checked_topic_part_count(0, 253).is_err());
        assert!(checked_topic_part_count(i32::MIN, i32::MAX).is_err());
        assert!(checked_topic_part_length(crate::utf16::EXCEL_STRING_LIMIT).is_ok());
        assert!(checked_topic_part_length(crate::utf16::EXCEL_STRING_LIMIT + 1).is_err());
    }

    #[test]
    fn topic_key_from_safearray_handles_single_and_rejects_multi_or_invalid_dimensions() {
        let _guard = TEST_LOCK.lock().unwrap();

        // 1. Single part SAFEARRAY of VARIANT BSTR.
        let bound = SAFEARRAYBOUND {
            cElements: 1,
            lLbound: 0,
        };

        // SAFETY: `bound` describes a valid one-dimensional VT_VARIANT
        // SAFEARRAY and remains readable for the call.
        let array = unsafe { SafeArrayCreate(VT_VARIANT, 1, &bound) };
        assert!(!array.is_null());

        let bstr_val = crate::utf16::encode_bounded("topic_one", "test", 100).unwrap();

        // SAFETY: `bstr_val` is readable for `bstr_val.len()` UTF-16 code units.
        let bstr = unsafe { SysAllocStringLen(bstr_val.as_ptr(), bstr_val.len() as u32) };

        let mut var = VARIANT::default();

        // SAFETY: `array` is live, index zero is in bounds, and `var` is
        // initialized as VT_BSTR. SafeArrayPutElement copies the VARIANT before
        // VariantClear releases the local BSTR.
        unsafe {
            var.Anonymous.Anonymous.vt = VT_BSTR;
            var.Anonymous.Anonymous.Anonymous.bstrVal = bstr;
            let index = 0i32;
            SafeArrayPutElement(array, &index, (&mut var as *mut VARIANT).cast());
            VariantClear(&mut var);
        }

        let mut array_ptr = array;

        // SAFETY: `array_ptr` points to a live SAFEARRAY variable. The function
        // reads the SAFEARRAY but does not take ownership of it.
        let key = unsafe { topic_key_from_safearray(&mut array_ptr) }.unwrap();
        assert_eq!(key, "topic_one");

        // SAFETY: `array` remains owned by this test and is destroyed exactly once.
        unsafe { SafeArrayDestroy(array) };

        // 2. Multi-part SAFEARRAYs are rejected because the COM topic key is
        // always one opaque string. Keeping one representation avoids topic
        // identity collisions between arities.
        let mut bounds = [SAFEARRAYBOUND {
            cElements: 2,
            lLbound: 0,
        }];

        // SAFETY: `bounds` describes a valid one-dimensional VT_VARIANT
        // SAFEARRAY and remains readable for the call.
        let array_multi = unsafe { SafeArrayCreate(VT_VARIANT, 1, bounds.as_mut_ptr()) };
        assert!(!array_multi.is_null());

        for (i, p) in ["part1", "part2"].iter().enumerate() {
            let u16_val = crate::utf16::encode_bounded(p, "test", 100).unwrap();

            // SAFETY: `u16_val` is readable for `u16_val.len()` UTF-16 units.
            let bstr = unsafe { SysAllocStringLen(u16_val.as_ptr(), u16_val.len() as u32) };

            let mut var = VARIANT::default();

            // SAFETY: `array_multi` is live, `i` is within its two-element
            // bounds, and `var` is initialized as VT_BSTR. SafeArrayPutElement
            // copies the VARIANT before VariantClear releases the local BSTR.
            unsafe {
                var.Anonymous.Anonymous.vt = VT_BSTR;
                var.Anonymous.Anonymous.Anonymous.bstrVal = bstr;
                let index = i as i32;
                SafeArrayPutElement(array_multi, &index, (&mut var as *mut VARIANT).cast());
                VariantClear(&mut var);
            }
        }

        let mut array_multi_ptr = array_multi;

        // SAFETY: `array_multi_ptr` points to a live SAFEARRAY variable. The
        // function reads but does not take ownership of the array.
        assert!(unsafe { topic_key_from_safearray(&mut array_multi_ptr) }.is_err());

        // SAFETY: `array_multi` remains owned by this test and is destroyed once.
        unsafe { SafeArrayDestroy(array_multi) };

        // 3. Multi-dimensional SAFEARRAY should fail validation.
        let mut bounds_2d = [
            SAFEARRAYBOUND {
                cElements: 2,
                lLbound: 0,
            },
            SAFEARRAYBOUND {
                cElements: 2,
                lLbound: 0,
            },
        ];

        // SAFETY: `bounds_2d` describes a valid two-dimensional VT_VARIANT
        // SAFEARRAY and remains readable for the call.
        let array_2d = unsafe { SafeArrayCreate(VT_VARIANT, 2, bounds_2d.as_mut_ptr()) };
        assert!(!array_2d.is_null());

        let mut array_2d_ptr = array_2d;

        // SAFETY: `array_2d_ptr` points to a live SAFEARRAY variable. The
        // function only inspects it and is expected to reject its dimensions.
        assert!(unsafe { topic_key_from_safearray(&mut array_2d_ptr) }.is_err());

        // SAFETY: `array_2d` remains owned by this test and is destroyed once.
        unsafe { SafeArrayDestroy(array_2d) };
    }

    #[test]
    fn standard_com_activation_exposes_unknown_dispatch_and_rtd_server() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        assert!(ensured.newly_created);

        // SAFETY: ACTIVE_SERVER and `ensured` retain the RTD server while the
        // factory and queried interfaces are exercised and released.
        let factory = get_test_class_factory(&ensured.active);
        let unknown_iid = iid_iunknown_from_fields();
        let mut server_unknown = ptr::null_mut();

        // SAFETY: `factory` is a live IClassFactory, aggregation is not
        // requested, and both the independent IID and output remain live.
        assert_eq!(
            // SAFETY: see the pointer and lifetime justification above.
            unsafe {
                (factory.vtable().create_instance)(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &unknown_iid,
                    &mut server_unknown,
                )
            },
            S_OK
        );
        let server_unknown = TestUnknownReference::new(server_unknown);

        // Query through the returned IUnknown vtable, as a COM client does,
        // using independently field-constructed standard/Excel IIDs.
        // SAFETY: CreateInstance returned a live IUnknown-compatible pointer.
        let unknown_vtable = server_unknown.iunknown_vtable();
        for iid in [
            iid_iunknown_from_fields(),
            iid_idispatch_from_fields(),
            iid_irtd_server_from_fields(),
        ] {
            let mut queried = ptr::null_mut();
            // SAFETY: `server_unknown` is live, `iid` is readable, and
            // `queried` is a writable output slot.
            assert_eq!(
                // SAFETY: see the pointer and lifetime justification above.
                unsafe {
                    ((*unknown_vtable).QueryInterface)(server_unknown.as_ptr(), &iid, &mut queried)
                },
                S_OK
            );
            let _queried = TestUnknownReference::new(queried);
        }

        drop(server_unknown);
        drop(factory);

        shutdown(handles).unwrap();
    }

    #[test]
    fn create_instance_nulls_output_on_every_rejected_request() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for the test.
        let factory = get_test_class_factory(&ensured.active);
        // SAFETY: `factory` is a live IClassFactory pointer.
        let create_instance = factory.vtable().create_instance;
        let unknown_iid = iid_iunknown_from_fields();
        let unsupported_iid = GUID {
            data1: 0xdead_beef,
            data2: 0xcafe,
            data3: 0x4000,
            data4: [0x80, 0, 1, 2, 3, 4, 5, 6],
        };
        let stale = NonNull::<u8>::dangling().as_ptr().cast::<c_void>();

        let mut output = stale;
        // SAFETY: non-null `outer` intentionally requests unsupported
        // aggregation; the other pointers are valid.
        assert_eq!(
            // SAFETY: see the intentional failure-case justification above.
            unsafe { create_instance(factory.as_ptr(), stale, &unknown_iid, &mut output) },
            CLASS_E_NOAGGREGATION
        );
        assert!(output.is_null());

        output = stale;
        // SAFETY: null `this` intentionally exercises pointer validation.
        assert_eq!(
            // SAFETY: the method validates `this` before dereferencing it.
            unsafe { create_instance(ptr::null_mut(), ptr::null_mut(), &unknown_iid, &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        // SAFETY: null IID intentionally exercises pointer validation.
        assert_eq!(
            // SAFETY: the method validates the IID before dereferencing it.
            unsafe { create_instance(factory.as_ptr(), ptr::null_mut(), ptr::null(), &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        // SAFETY: the unsupported IID is readable and output is writable.
        assert_eq!(
            // SAFETY: all pointers are live for the call.
            unsafe {
                create_instance(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &unsupported_iid,
                    &mut output,
                )
            },
            E_NOINTERFACE
        );
        assert!(output.is_null());

        // SAFETY: null output intentionally exercises pointer validation.
        assert_eq!(
            // SAFETY: the method validates output before dereferencing it.
            unsafe {
                create_instance(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &unknown_iid,
                    ptr::null_mut(),
                )
            },
            E_POINTER
        );

        drop(factory);
        shutdown(handles).unwrap();
    }

    #[test]
    fn com_query_failures_clear_stale_output_pointers() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let class_factory_iid = iid_iclass_factory_from_fields();
        let unknown_iid = iid_iunknown_from_fields();
        let unsupported_iid = GUID {
            data1: 0x7654_3210,
            data2: 0xabcd,
            data3: 0x4000,
            data4: [0x80, 1, 2, 3, 4, 5, 6, 7],
        };
        let stale = NonNull::<u8>::dangling().as_ptr().cast::<c_void>();

        let mut output = stale;
        assert_eq!(
            // SAFETY: the null class pointer intentionally exercises
            // validation; the IID and output slot remain live.
            unsafe {
                dll_get_class_object(
                    ptr::null(),
                    (&class_factory_iid as *const GUID).cast(),
                    &mut output,
                )
            },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: the class ID and output are live; the null IID
            // intentionally exercises validation.
            unsafe {
                dll_get_class_object(
                    (&ensured.active.class_id as *const GUID).cast(),
                    ptr::null(),
                    &mut output,
                )
            },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: both GUIDs and the output slot remain live. The
            // unsupported IID intentionally forces factory QI failure.
            unsafe {
                dll_get_class_object(
                    (&ensured.active.class_id as *const GUID).cast(),
                    (&unsupported_iid as *const GUID).cast(),
                    &mut output,
                )
            },
            E_NOINTERFACE
        );
        assert!(output.is_null());

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for the test.
        let factory = get_test_class_factory(&ensured.active);
        // SAFETY: `factory` is a live IClassFactory pointer.
        let factory_query = factory.vtable().query_interface;

        output = stale;
        assert_eq!(
            // SAFETY: null `this` intentionally exercises validation; the IID
            // and output slot remain live.
            unsafe { factory_query(ptr::null_mut(), &unknown_iid, &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: `factory` and output are live; null IID intentionally
            // exercises validation.
            unsafe { factory_query(factory.as_ptr(), ptr::null(), &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: all pointers are live and the IID is intentionally
            // unsupported.
            unsafe { factory_query(factory.as_ptr(), &unsupported_iid, &mut output) },
            E_NOINTERFACE
        );
        assert!(output.is_null());

        let mut server_unknown = ptr::null_mut();
        assert_eq!(
            // SAFETY: `factory`, the IID, and output slot remain live.
            unsafe {
                (factory.vtable().create_instance)(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &unknown_iid,
                    &mut server_unknown,
                )
            },
            S_OK
        );
        let server_unknown = TestUnknownReference::new(server_unknown);
        let server = server_unknown.cast::<RtdServer>();
        // SAFETY: CreateInstance returned the RtdServer identity pointer.
        let server_query = unsafe { (*server.as_ref().vtable).query_interface };

        output = stale;
        assert_eq!(
            // SAFETY: null `this` intentionally exercises validation; the IID
            // and output remain live.
            unsafe { server_query(ptr::null_mut(), &unknown_iid, &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: `server` and output remain live; null IID intentionally
            // exercises validation.
            unsafe { server_query(server.as_ptr(), ptr::null(), &mut output) },
            E_POINTER
        );
        assert!(output.is_null());

        output = stale;
        assert_eq!(
            // SAFETY: all pointers remain live and the IID is intentionally
            // unsupported.
            unsafe { server_query(server.as_ptr(), &unsupported_iid, &mut output) },
            E_NOINTERFACE
        );
        assert!(output.is_null());

        drop(server_unknown);
        drop(factory);
        shutdown(handles).unwrap();
    }

    #[test]
    fn idispatch_resolves_names_and_invokes_heartbeat() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server while its COM
        // interfaces are used below.
        let factory = get_test_class_factory(&ensured.active);
        let dispatch_iid = iid_idispatch_from_fields();
        let mut dispatch = ptr::null_mut();
        assert_eq!(
            // SAFETY: `factory`, the IID, and output are live for the call.
            unsafe {
                (factory.vtable().create_instance)(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &dispatch_iid,
                    &mut dispatch,
                )
            },
            S_OK
        );
        let dispatch = TestUnknownReference::new(dispatch);
        let server = dispatch.cast::<RtdServer>();
        // SAFETY: the IDispatch pointer is the RtdServer's identity pointer.
        let vtable = unsafe { server.as_ref().vtable };

        let mut type_info_count = u32::MAX;
        assert_eq!(
            // SAFETY: `server` is live and the count output is writable.
            unsafe { ((*vtable).get_type_info_count)(server.as_ptr(), &mut type_info_count) },
            S_OK
        );
        assert_eq!(type_info_count, 0);

        let stale = NonNull::<u8>::dangling().as_ptr().cast::<c_void>();
        let mut type_info = stale;
        assert_eq!(
            // SAFETY: `server` is live and the output is writable.
            unsafe { ((*vtable).get_type_info)(server.as_ptr(), 0, 0, &mut type_info) },
            E_NOTIMPL
        );
        assert!(type_info.is_null());
        type_info = stale;
        assert_eq!(
            // SAFETY: `server` is live and the output is writable.
            unsafe { ((*vtable).get_type_info)(server.as_ptr(), 1, 0, &mut type_info) },
            DISP_E_BADINDEX
        );
        assert!(type_info.is_null());

        let null_iid = iid_null_from_fields();
        for (name, expected) in [
            ("serverstart", DISPID_SERVER_START),
            ("CONNECTDATA", DISPID_CONNECT_DATA),
            ("RefreshData", DISPID_REFRESH_DATA),
            ("disconnectdata", DISPID_DISCONNECT_DATA),
            ("hEaRtBeAt", DISPID_HEARTBEAT),
            ("ServerTerminate", DISPID_SERVER_TERMINATE),
        ] {
            let name = wide_nul(name);
            let names = [name.as_ptr()];
            let mut id = DISPID_UNKNOWN;
            assert_eq!(
                // SAFETY: all COM input and output arrays remain live.
                unsafe {
                    ((*vtable).get_ids_of_names)(
                        server.as_ptr(),
                        &null_iid,
                        names.as_ptr(),
                        1,
                        0,
                        &mut id,
                    )
                },
                S_OK
            );
            assert_eq!(id, expected);
        }

        let member = wide_nul("connectdata");
        let topic = wide_nul("TOPICid");
        let strings = wide_nul("strings");
        let new_values = wide_nul("getnewvalues");
        let names = [
            member.as_ptr(),
            topic.as_ptr(),
            strings.as_ptr(),
            new_values.as_ptr(),
        ];
        let mut ids = [99; 4];
        assert_eq!(
            // SAFETY: all COM input and output arrays remain live.
            unsafe {
                ((*vtable).get_ids_of_names)(
                    server.as_ptr(),
                    &null_iid,
                    names.as_ptr(),
                    names.len() as u32,
                    0,
                    ids.as_mut_ptr(),
                )
            },
            S_OK
        );
        assert_eq!(ids, [DISPID_CONNECT_DATA, 0, 1, 2]);

        let unknown = wide_nul("notAnRtdMember");
        let names = [unknown.as_ptr()];
        let mut id = 123;
        assert_eq!(
            // SAFETY: all COM input and output arrays remain live.
            unsafe {
                ((*vtable).get_ids_of_names)(
                    server.as_ptr(),
                    &null_iid,
                    names.as_ptr(),
                    1,
                    0,
                    &mut id,
                )
            },
            DISP_E_UNKNOWNNAME
        );
        assert_eq!(id, DISPID_UNKNOWN);

        let mut parameters = DISPPARAMS::default();
        let mut result = VARIANT::default();
        let mut exception = EXCEPINFO::default();
        let mut argument_error = u32::MAX;
        assert_eq!(
            // SAFETY: the server, IID, DISPPARAMS, and outputs remain live.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_HEARTBEAT,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    &mut exception,
                    &mut argument_error,
                )
            },
            S_OK
        );
        // SAFETY: successful Invoke initialized `result` as VT_I4; clearing it
        // balances any owned Automation payload (none for this scalar).
        unsafe {
            assert_eq!(result.Anonymous.Anonymous.vt, VT_I4);
            assert_eq!(result.Anonymous.Anonymous.Anonymous.lVal, 1);
            VariantClear(&mut result);
        }

        assert_eq!(
            // SAFETY: the server, IID, and empty DISPPARAMS remain live.
            // IDispatch explicitly permits callers to ignore a return value by
            // passing a null pVarResult.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_HEARTBEAT,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            S_OK
        );

        drop(dispatch);
        drop(factory);
        shutdown(handles).unwrap();
    }

    #[test]
    fn idispatch_validates_flags_counts_types_and_reversed_arguments() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let ensured = ensure_server(Some(Arc::clone(&handles)), None).unwrap();

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server for the test.
        let factory = get_test_class_factory(&ensured.active);
        let dispatch_iid = iid_idispatch_from_fields();
        let mut dispatch = ptr::null_mut();
        assert_eq!(
            // SAFETY: `factory`, the IID, and output are live for the call.
            unsafe {
                (factory.vtable().create_instance)(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &dispatch_iid,
                    &mut dispatch,
                )
            },
            S_OK
        );
        let dispatch = TestUnknownReference::new(dispatch);
        let server = dispatch.cast::<RtdServer>();
        // SAFETY: the IDispatch pointer is the RtdServer's identity pointer.
        let vtable = unsafe { server.as_ref().vtable };
        let null_iid = iid_null_from_fields();

        let mut parameters = DISPPARAMS::default();
        let mut result = VARIANT::default();
        result.Anonymous.Anonymous.vt = VT_I4;
        result.Anonymous.Anonymous.Anonymous.lVal = 99;
        let mut exception = EXCEPINFO {
            scode: 99,
            ..EXCEPINFO::default()
        };
        let mut argument_error = 99;
        assert_eq!(
            // SAFETY: all pointers remain live; zero arguments intentionally
            // exercise argument-count validation.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_DISCONNECT_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    &mut exception,
                    &mut argument_error,
                )
            },
            DISP_E_BADPARAMCOUNT
        );
        // SAFETY: Invoke initialized the result before rejecting the call.
        unsafe { assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY) };
        assert_eq!(exception.scode, 0);
        assert_eq!(argument_error, 0);

        let mut bad_argument = VARIANT::default();
        parameters.rgvarg = &mut bad_argument;
        parameters.cArgs = 1;
        assert_eq!(
            // SAFETY: the one-element argument array and all outputs are live.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_DISCONNECT_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    ptr::null_mut(),
                    &mut argument_error,
                )
            },
            DISP_E_TYPEMISMATCH
        );
        assert_eq!(argument_error, 0);

        assert_eq!(
            // SAFETY: the temporary empty DISPPARAMS and result remain live for
            // the call; flags intentionally omit DISPATCH_METHOD.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_HEARTBEAT,
                    &null_iid,
                    0,
                    0,
                    &mut DISPPARAMS::default(),
                    &mut result,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            DISP_E_MEMBERNOTFOUND
        );
        // SAFETY: Invoke initialized the result before rejecting the flags.
        unsafe { assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY) };

        let topic_array = {
            let bound = SAFEARRAYBOUND {
                cElements: 1,
                lLbound: 0,
            };
            // SAFETY: `bound` describes a one-element VT_VARIANT SAFEARRAY.
            let array = unsafe { SafeArrayCreate(VT_VARIANT, 1, &bound) };
            assert!(!array.is_null());

            let mut topic = VARIANT::default();
            assert_eq!(
                // SAFETY: `topic` is writable and the string is valid UTF-8.
                unsafe { write_bstr_variant(&mut topic, "invalid-topic") },
                S_OK
            );
            let index = 0;
            // SAFETY: the index is within bounds and SafeArrayPutElement copies
            // the valid VARIANT before the local is cleared.
            assert_eq!(
                // SAFETY: see the bounds and lifetime justification above.
                unsafe { SafeArrayPutElement(array, &index, (&mut topic as *mut VARIANT).cast(),) },
                S_OK
            );
            // SAFETY: `topic` contains one owned BSTR initialized above.
            unsafe { VariantClear(&mut topic) };
            array
        };

        let mut typed_array = topic_array;
        let mut typed_new_values = VARIANT_TRUE;
        let mut typed_result = VARIANT::default();
        typed_result.Anonymous.Anonymous.vt = VT_I4;
        typed_result.Anonymous.Anonymous.Anonymous.lVal = 99;
        assert_eq!(
            // SAFETY: the live server, SAFEARRAY pointer, and writable outputs
            // remain valid for this direct typed-vtable failure case.
            unsafe {
                connect_data(
                    server.as_ptr(),
                    41,
                    &mut typed_array,
                    &mut typed_new_values,
                    &mut typed_result,
                )
            },
            E_INVALIDARG
        );
        assert_eq!(typed_new_values, VARIANT_FALSE);
        // SAFETY: ConnectData initializes the out VARIANT before every early
        // failure after pointer validation.
        unsafe { assert_eq!(typed_result.Anonymous.Anonymous.vt, VT_EMPTY) };

        let mut new_values = VARIANT_TRUE;
        let mut reversed = [VARIANT::default(), VARIANT::default(), VARIANT::default()];
        // DISPPARAMS stores positional arguments in reverse signature order:
        // GetNewValues, Strings, TopicID.
        reversed[0].Anonymous.Anonymous.vt = VT_BYREF | VT_BOOL;
        reversed[0].Anonymous.Anonymous.Anonymous.pboolVal = &mut new_values;
        reversed[1].Anonymous.Anonymous.vt = VT_ARRAY | VT_VARIANT;
        reversed[1].Anonymous.Anonymous.Anonymous.parray = topic_array;
        reversed[2].Anonymous.Anonymous.vt = VT_I4;
        reversed[2].Anonymous.Anonymous.Anonymous.lVal = 42;
        parameters = DISPPARAMS {
            rgvarg: reversed.as_mut_ptr(),
            rgdispidNamedArgs: ptr::null_mut(),
            cArgs: 3,
            cNamedArgs: 0,
        };
        assert_eq!(
            // SAFETY: the reversed three-element argument array and outputs
            // remain live for Invoke.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_CONNECT_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    ptr::null_mut(),
                    &mut argument_error,
                )
            },
            E_INVALIDARG
        );
        assert_eq!(new_values, VARIANT_FALSE);
        // SAFETY: Invoke reset `result` to empty; reversed[1] still owns the
        // SAFEARRAY created above and destroys it exactly once.
        unsafe {
            assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY);
            // This VARIANT owns `topic_array` and destroys it exactly once.
            VariantClear(&mut reversed[1]);
        }

        // A named positional parameter uses the stable parameter DISPID
        // returned by GetIDsOfNames.
        let mut topic_id = VARIANT::default();
        topic_id.Anonymous.Anonymous.vt = VT_I4;
        topic_id.Anonymous.Anonymous.Anonymous.lVal = 42;
        let mut named_id = 0;
        parameters = DISPPARAMS {
            rgvarg: &mut topic_id,
            rgdispidNamedArgs: &mut named_id,
            cArgs: 1,
            cNamedArgs: 1,
        };
        assert_eq!(
            // SAFETY: the named argument, its DISPID, and outputs remain live.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_DISCONNECT_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    ptr::null_mut(),
                    &mut argument_error,
                )
            },
            S_OK
        );
        // SAFETY: successful void Invoke leaves the initialized result empty.
        unsafe { assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY) };

        // SAFETY: CreateInstance and DllGetClassObject returned these owned
        // references.
        drop(dispatch);
        drop(factory);
        shutdown(handles).unwrap();
    }

    #[test]
    fn idispatch_refresh_transfers_safearray_and_terminate_quiesces_subscription() {
        let _guard = TEST_LOCK.lock().unwrap();
        let subscriptions = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(DispatchTestSource {
            sink: Mutex::new(None),
            disconnected: Arc::clone(&disconnected),
        });
        let ensured = ensure_server(None, Some(Arc::clone(&subscriptions))).unwrap();
        let _generation = ensured.active.generation;
        let handle = ensured.subscription_server.as_ref().unwrap().clone();

        let prepared = subscriptions
            .prepare(
                Arc::clone(&source),
                RtdTopic::single("dispatch-refresh").unwrap(),
            )
            .unwrap();
        let key_obj = prepared.key().clone();
        let conn = subscriptions
            .connect_transaction(&handle, crate::subscription::TopicId(77), &key_obj)
            .unwrap();
        assert_eq!(conn.value(), &RtdValue::Number(12.5));
        conn.commit().unwrap();
        drop(prepared);
        assert_eq!(handle.pending_update_count(), 0);

        // SAFETY: ACTIVE_SERVER and `ensured` retain the server while the
        // factory and dispatch interface are used.
        let factory = get_test_class_factory(&ensured.active);
        let dispatch_iid = iid_idispatch_from_fields();
        let mut dispatch = ptr::null_mut();
        assert_eq!(
            // SAFETY: `factory`, the IID, and output slot remain live.
            unsafe {
                (factory.vtable().create_instance)(
                    factory.as_ptr(),
                    ptr::null_mut(),
                    &dispatch_iid,
                    &mut dispatch,
                )
            },
            S_OK
        );
        let dispatch = TestUnknownReference::new(dispatch);
        let server = dispatch.cast::<RtdServer>();
        // SAFETY: CreateInstance returned the RtdServer identity pointer.
        let vtable = unsafe { server.as_ref().vtable };
        let null_iid = iid_null_from_fields();

        let mut topic_count = -1;
        let mut count_argument = VARIANT::default();
        count_argument.Anonymous.Anonymous.vt = VT_BYREF | VT_I4;
        count_argument.Anonymous.Anonymous.Anonymous.plVal = &mut topic_count;
        let mut parameters = DISPPARAMS {
            rgvarg: &mut count_argument,
            rgdispidNamedArgs: ptr::null_mut(),
            cArgs: 1,
            cNamedArgs: 0,
        };
        let mut result = VARIANT::default();

        // The synchronous initial publish is acknowledged by connection commit
        // and therefore is not a pending RefreshData row.
        source.publish(13.5).unwrap();
        assert!(handle.pending_update_count() > 0);

        assert_eq!(
            // SAFETY: the server, IID, one-element argument array, and result
            // remain live for Invoke.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_REFRESH_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            S_OK
        );
        assert_eq!(topic_count, 1);

        // SAFETY: successful RefreshData initialized result as a
        // VT_ARRAY|VT_VARIANT and transferred one owned SAFEARRAY into it.
        let array = unsafe {
            assert_eq!(result.Anonymous.Anonymous.vt, VT_ARRAY | VT_VARIANT);
            result.Anonymous.Anonymous.Anonymous.parray
        };
        assert!(!array.is_null());
        // SAFETY: `array` remains owned by `result` and is live until the
        // VariantClear below.
        assert_eq!(unsafe { SafeArrayGetDim(array) }, 2);

        let mut topic = VARIANT::default();
        let mut value = VARIANT::default();
        let mut topic_index = [0, 0];
        let mut value_index = [1, 0];
        // SAFETY: both indices lie inside the one-column, two-row array, and
        // the VARIANT outputs are writable.
        unsafe {
            assert_eq!(
                SafeArrayGetElement(
                    array,
                    topic_index.as_mut_ptr(),
                    (&mut topic as *mut VARIANT).cast(),
                ),
                S_OK
            );
            assert_eq!(
                SafeArrayGetElement(
                    array,
                    value_index.as_mut_ptr(),
                    (&mut value as *mut VARIANT).cast(),
                ),
                S_OK
            );
            assert_eq!(topic.Anonymous.Anonymous.vt, VT_I4);
            assert_eq!(topic.Anonymous.Anonymous.Anonymous.lVal, 77);
            assert_eq!(value.Anonymous.Anonymous.vt, VT_R8);
            assert_eq!(value.Anonymous.Anonymous.Anonymous.dblVal, 13.5);
            VariantClear(&mut topic);
            VariantClear(&mut value);
            // This is the sole owner of the transferred SAFEARRAY.
            VariantClear(&mut result);
        }
        assert_eq!(handle.pending_update_count(), 0);

        source.publish(14.5).unwrap();
        assert!(handle.pending_update_count() > 0);
        topic_count = -1;
        assert_eq!(
            // SAFETY: all inputs remain live. A null pVarResult asks Invoke to
            // discard the returned SAFEARRAY after committing the update.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_REFRESH_DATA,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            S_OK
        );
        assert_eq!(topic_count, 1);
        assert_eq!(handle.pending_update_count(), 0);

        parameters = DISPPARAMS::default();
        result.Anonymous.Anonymous.vt = VT_I4;
        result.Anonymous.Anonymous.Anonymous.lVal = 123;
        assert_eq!(
            // SAFETY: the server remains live through its dispatch reference;
            // ServerTerminate takes no arguments and initializes the result
            // before quiescing the generation.
            unsafe {
                ((*vtable).invoke)(
                    server.as_ptr(),
                    DISPID_SERVER_TERMINATE,
                    &null_iid,
                    0,
                    DISPATCH_METHOD,
                    &mut parameters,
                    &mut result,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            S_OK
        );
        // SAFETY: successful void Invoke leaves the initialized result empty.
        unsafe { assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY) };
        assert!(disconnected.load(Ordering::Acquire));

        drop(dispatch);
        drop(factory);
        subscriptions.close().unwrap();
    }

    #[test]
    fn wrong_clsid_is_not_served() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let _active = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        let wrong = GUID::from_u128(1);
        let class_factory_iid = iid_iclass_factory_from_fields();
        let mut output = ptr::null_mut();

        // SAFETY: the input pointers reference live GUID values and `output`
        // points to a writable COM interface output slot.
        let status = unsafe {
            dll_get_class_object(
                (&wrong as *const GUID).cast(),
                (&class_factory_iid as *const GUID).cast(),
                &mut output,
            )
        };

        assert_eq!(status, CLASS_E_CLASSNOTAVAILABLE);
        assert!(output.is_null());

        shutdown(handles).unwrap();
    }

    #[test]
    fn existing_server_attaches_each_backend_without_replacement() {
        let _guard = TEST_LOCK.lock().unwrap();
        let handles = Arc::new(HandleRuntime::new(4));
        let subscriptions = Arc::new(SubscriptionRuntime::new());

        let first = ensure_server(Some(Arc::clone(&handles)), None).unwrap();
        assert!(first.newly_created);

        let second = ensure_server(None, Some(Arc::clone(&subscriptions))).unwrap();
        assert!(!second.newly_created);
        assert_eq!(first.active.pointer, second.active.pointer);

        let server = second.active.pointer as *mut RtdServer;

        // SAFETY: ACTIVE_SERVER owns a live reference and both EnsuredServer
        // guards retain additional temporary references for this test.
        let backends = unsafe { (*server).backends.lock() };

        assert!(
            backends
                .handles
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &handles))
        );
        assert!(
            backends
                .subscriptions
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &subscriptions))
        );
    }

    #[test]
    fn repeated_ensure_server_calls_do_not_rearm_subscription_notifications() {
        use std::sync::atomic::AtomicUsize;

        let _guard = TEST_LOCK.lock().unwrap();
        let subscriptions = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));

        let ensured = ensure_server(None, Some(Arc::clone(&subscriptions))).unwrap();
        let server = ensured.active.pointer as *mut RtdServer;

        let callback = Arc::new(RetainedUpdateCallback {
            cookie: None,
            drop_hook: None,
        });
        // SAFETY: EnsuredServer keeps server reference alive
        unsafe {
            install_callback(&(*server).callbacks, callback);
        }

        let handle = ensured.subscription_server.as_ref().unwrap();
        handle
            .attach_update_callback({
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let (source, sink, _) = crate::subscription::tests::publishing_source(None);
        let prepared = subscriptions
            .prepare(source, RtdTopic::single("ensure-test").unwrap())
            .unwrap();
        let key_obj = prepared.key().clone();
        prepared.commit();
        let conn = subscriptions
            .connect_transaction(handle, crate::subscription::TopicId(1), &key_obj)
            .unwrap();
        conn.commit().unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();

        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        for _ in 0..100 {
            let _res = ensure_server(None, Some(Arc::clone(&subscriptions))).unwrap();
        }

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        drop(ensured);
        shutdown_subscriptions(subscriptions).unwrap();
    }

    #[test]
    fn temporary_registration_mutex_serializes_other_threads() {
        let _guard = TEST_LOCK.lock().unwrap();
        let name = format!("Local\\XlFnRtdRegistrationTest_{}", std::process::id());
        let first = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _second = CrossProcessRegistrationGuard::acquire_named(&name).unwrap();
            acquired_tx.send(()).unwrap();
        });

        assert!(
            acquired_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        drop(first);
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn scavenger_deletes_only_fully_marked_registration_for_same_module() {
        let _guard = TEST_LOCK.lock().unwrap();
        let _maintenance = REGISTRATION_MAINTENANCE.lock();
        let mut owned_id = GUID::from_u128(0);
        let mut foreign_id = GUID::from_u128(0);
        let mut legacy_id = GUID::from_u128(0);

        // SAFETY: each argument points to distinct writable GUID storage.
        unsafe {
            assert!(CoCreateGuid(&mut owned_id) >= 0);
            assert!(CoCreateGuid(&mut foreign_id) >= 0);
            assert!(CoCreateGuid(&mut legacy_id) >= 0);
        }

        let owned_class = guid_braced(owned_id);
        let foreign_class = guid_braced(foreign_id);
        let legacy_class = guid_braced(legacy_id);
        let owned_prog_id = format!("{RTD_PROG_ID_PREFIX}{}", guid_compact(owned_id));
        let foreign_prog_id = format!("{RTD_PROG_ID_PREFIX}{}", guid_compact(foreign_id));
        let legacy_prog_id = format!("{RTD_PROG_ID_PREFIX}{}", guid_compact(legacy_id));
        let owned_key = format!("Software\\Classes\\{owned_prog_id}");
        let foreign_key = format!("Software\\Classes\\{foreign_prog_id}");
        let legacy_key = format!("Software\\Classes\\{legacy_prog_id}");
        let module = r"C:\xlfn-tests\owned.xll";

        for (key, class, owner, schema) in [
            (
                &owned_key,
                &owned_class,
                RTD_REGISTRATION_OWNER,
                RTD_REGISTRATION_SCHEMA,
            ),
            (
                &foreign_key,
                &foreign_class,
                "another-owner",
                RTD_REGISTRATION_SCHEMA,
            ),
            (&legacy_key, &legacy_class, RTD_REGISTRATION_OWNER, "1"),
        ] {
            set_registry_value(key, Some("XlFnOwner"), owner).unwrap();
            set_registry_value(key, Some("XlFnRegistrationSchema"), schema).unwrap();
            set_registry_value(key, Some("XlFnOwnerModule"), module).unwrap();
            set_registry_value(key, Some("XlFnClassId"), class).unwrap();
        }

        scavenge_owned_registrations(module, None).unwrap();

        assert!(
            read_registry_string(&owned_key, "XlFnOwner")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            read_registry_string(&foreign_key, "XlFnOwner").unwrap(),
            Some("another-owner".to_owned())
        );
        assert_eq!(
            read_registry_string(&legacy_key, "XlFnRegistrationSchema").unwrap(),
            Some("1".to_owned())
        );

        for key in [&foreign_key, &legacy_key] {
            let key = wide_nul(key);
            // SAFETY: `key` is an exact NUL-terminated test-owned registry key
            // created above and may be deleted during test cleanup.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
        }
    }
}
