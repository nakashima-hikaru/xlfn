use super::automation::{
    server_get_ids_of_names, server_invoke, topic_key_from_safearray, write_bstr_variant,
    write_refresh_data,
};
use super::com_abi::IID_IUNKNOWN;
use super::com_abi::com_lifetime_boundary;
use super::global_interface_table::get_git;
use super::module_state::{ComObjectKind, ComObjectLease};
use super::registration::guid_compact;
use super::server_gate::{
    DeferredTerminationTicket, DrainedServerOperations, OwnedServerCall,
    OwnedServerObservationCall, ServerCloseError, ServerOperationBarrier, ServerPhase,
    ServerResourceTeardown, ServerTermination, ServerTerminationRequest, TerminationWorker,
};
use super::subscription_backend::{
    BackendSubscriptionServer, BackendSubscriptions, finish_connect_data,
};
use super::update_event::{
    CallbackPtr, GitCookieLease, RetainedUpdateCallback, RtdNotifier, RtdUpdateEvent,
    ServerCallbacks, active_callback, drain_callbacks, install_callback, retry_git_revocation_debt,
};
use super::{com_boundary, guid_eq};
use crate::error::InputError;
use crate::formula_lifetime::{FormulaLifetimeBackend, FormulaLifetimeGeneration};
use crate::subscription::ServerGeneration;
use crate::subscription::SubscriptionRuntime;
use crate::sync::Mutex;
use crate::win32::E_UNEXPECTED;
use crate::win32::{
    CoCreateGuid, DISP_E_BADINDEX, DISPPARAMS, E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL,
    E_POINTER, EXCEPINFO, GUID, S_OK, SAFEARRAY, VARIANT, VARIANT_BOOL, VARIANT_FALSE,
    VARIANT_TRUE,
};
use crate::{RtdSubscription, XllError, XllResult};
use std::any::TypeId;
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use triomphe::Arc;

pub(super) const IID_IDISPATCH: GUID = crate::win32::IID_IDispatch;
pub(super) const IID_IRTD_SERVER: GUID = GUID {
    data1: 0xec0e6191,
    data2: 0xdb51,
    data3: 0x11d3,
    data4: [0x8f, 0x3e, 0x00, 0xc0, 0x4f, 0x36, 0x51, 0xb8],
};

pub(super) const IID_IRTD_UPDATE_EVENT: GUID = GUID {
    data1: 0xa43788c1,
    data2: 0xd91b,
    data3: 0x11d3,
    data4: [0x8f, 0x39, 0x00, 0xc0, 0x4f, 0x36, 0x51, 0xb8],
};
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
    pub(super) subscription_type: TypeId,
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

pub(super) struct EnsuredServer<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(super) active: ActiveServer,
    #[cfg(test)]
    pub(super) newly_created: bool,
    pub(super) subscription_server: Option<BackendSubscriptionServer<E, C>>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> EnsuredServer<E, C> {
    pub(super) fn pointer(&self) -> *mut RtdServer<E, C> {
        self.active.pointer as *mut RtdServer<E, C>
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop for EnsuredServer<E, C> {
    fn drop(&mut self) {
        // SAFETY: `ensure_server` acquires one temporary reference specifically
        // for this guard, so dropping the guard must release exactly that reference.
        unsafe { server_release(self.pointer()) };
    }
}

/// Owns one observation's temporary COM reference and operation admission.
/// Every fallible preparation and host invocation is inside this transaction.
/// A successful ServerStart is the COM publication boundary, independently of
/// Excel's later return status. Otherwise the last failed observation retires
/// the unused server through the normal terminal protocol.
pub(super) struct ServerObservation<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    ensured: EnsuredServer<E, C>,
    operation: Option<OwnedServerObservationCall>,
    committed: bool,
}

#[derive(Default)]
struct ObservationState {
    active: usize,
    committed: bool,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ServerObservation<E, C> {
    pub(super) fn begin(ensured: EnsuredServer<E, C>) -> XllResult<Self> {
        let pointer = ensured.active.pointer as *mut RtdServer<E, C>;
        let active = ACTIVE_SERVER.lock();
        if !active
            .as_ref()
            .is_some_and(|entry| entry.pointer == pointer as usize)
        {
            return Err(XllError::Closing);
        }
        // SAFETY: ensured owns a live COM reference through the guard's drop;
        // the owned admission is dropped before that reference.
        let operation =
            unsafe { (*pointer).operations.enter_observation() }.ok_or(XllError::Closing)?;
        // SAFETY: ensured retains the server and ACTIVE_SERVER serializes
        // observation admission with the last observer's retirement decision.
        unsafe {
            let mut observations = (*pointer).observations.lock();
            observations.active = observations
                .active
                .checked_add(1)
                .expect("RTD observation count cannot overflow");
        };
        drop(active);
        Ok(Self {
            ensured,
            operation: Some(operation),
            committed: false,
        })
    }

    pub(super) fn server(&self) -> &EnsuredServer<E, C> {
        &self.ensured
    }

    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop for ServerObservation<E, C> {
    fn drop(&mut self) {
        // Release observation admission before requesting termination. An
        // in-flight COM ServerStart still owns its separate admission.
        drop(self.operation.take());
        let pointer = self.ensured.active.pointer as *mut RtdServer<E, C>;
        let active = ACTIVE_SERVER.lock();
        // SAFETY: self.ensured retains the server throughout this destructor.
        let server = unsafe { &*pointer };
        let mut observations = server.observations.lock();
        observations.active -= 1;
        observations.committed |= self.committed;
        let retire = observations.active == 0
            && !observations.committed
            && server.start_state.load(Ordering::Acquire) != SERVER_STARTED
            && active
                .as_ref()
                .is_some_and(|entry| entry.pointer == pointer as usize);
        drop(observations);
        // Make retirement atomic with observation admission and ensure_server.
        // STARTING defers disposal until its admitted COM method has exited;
        // NOT_STARTED/START_FAILED normally permit synchronous disposal.
        let request = retire.then(|| server.operations.request_termination());
        drop(active);
        if let Some(request) = request {
            let status = match request {
                // SAFETY: ensured retains the server through completion and
                // the request owns its terminal barrier reservation.
                Ok(request) => unsafe { finish_termination_request(pointer, request) },
                Err(_) => E_FAIL,
            };
            if status != S_OK {
                crate::diagnostics::report_no_unwind(
                    "RTD failed observation teardown",
                    &XllError::WindowsApi {
                        function: "IRtdServer::ServerTerminate",
                        code: status,
                    },
                );
            }
        }
    }
}

#[repr(C)]
pub(super) struct RtdServer<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(super) vtable: *const RtdServerVtable<E, C>,
    pub(super) references: AtomicU32,
    start_state: AtomicU8,
    observations: Mutex<ObservationState>,
    pub(super) generation: ServerGeneration,
    // Notification workers own a clone so the barrier stays independently
    // allocated through their final rejected entry and thread exit.
    pub(super) operations: Arc<ServerOperationBarrier>,
    pub(super) termination_worker: TerminationWorker,
    pub(super) backends: Mutex<ServerBackends<E, C>>,
    pub(super) callbacks: Mutex<ServerCallbacks>,
    // Keep the module hold until every other field has been destroyed.
    pub(super) _module_lease: ComObjectLease,
}

/// Exclusive ServerStart reservation bound to its server before publication.
pub(super) struct ServerStartReservation<
    'a,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    server: Option<&'a RtdServer<E, C>>,
}

/// A callback has been installed on the reserved server; failure is terminal.
pub(super) struct PublishedServerStart<
    'a,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    server: Option<&'a RtdServer<E, C>>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdServer<E, C> {
    pub(super) fn reserve_start(&self) -> Option<ServerStartReservation<'_, E, C>> {
        self.start_state
            .compare_exchange(
                SERVER_NOT_STARTED,
                SERVER_STARTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| ServerStartReservation { server: Some(self) })
    }

    pub(super) fn start_state(&self) -> u8 {
        self.start_state.load(Ordering::Acquire)
    }
}

impl<'a, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ServerStartReservation<'a, E, C> {
    pub(super) fn publish_callback(
        mut self,
        callback: Box<RetainedUpdateCallback>,
    ) -> (PublishedServerStart<'a, E, C>, CallbackPtr) {
        let server = self.server.expect("ServerStart owns its target");
        let callback = install_callback(&server.callbacks, callback);
        let server = self.server.take().expect("ServerStart owns its target");
        // Once the callback is server-owned, retry on this server is terminal.
        (
            PublishedServerStart {
                server: Some(server),
            },
            callback,
        )
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> PublishedServerStart<'_, E, C> {
    pub(super) fn commit(mut self) {
        self.server
            .take()
            .expect("published ServerStart owns its target")
            .start_state
            .store(SERVER_STARTED, Ordering::Release);
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop for ServerStartReservation<'_, E, C> {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server
                .start_state
                .store(SERVER_NOT_STARTED, Ordering::Release);
        }
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop for PublishedServerStart<'_, E, C> {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server
                .start_state
                .store(SERVER_START_FAILED, Ordering::Release);
        }
    }
}

struct OwnedServerReference {
    pub(super) pointer: usize,
}

pub(crate) struct DeferredDisconnectLease {
    // Signal COM quiescence before releasing the reference that owns the
    // barrier. Shutdown pumps STA dispatch while waiting for this admission.
    _call: OwnedServerCall,
    _reference: OwnedServerReference,
}

impl OwnedServerReference {
    unsafe fn acquire<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
        pointer: *mut RtdServer<E, C>,
    ) -> Self {
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
        unsafe {
            let pointer = self.pointer as *mut c_void;
            let vtable = *pointer.cast::<*const super::com_abi::IUnknown_Vtbl>();
            ((*vtable).Release)(pointer);
        };
    }
}

#[derive(Clone, Copy)]
pub(super) struct BackendHandles(pub(super) NonNull<FormulaLifetimeBackend>);

// SAFETY: FormulaLifetimeBackend is Send + Sync and remains valid while attached to the server.
unsafe impl Send for BackendHandles where FormulaLifetimeBackend: Send + Sync {}
// SAFETY: FormulaLifetimeBackend immutable operations are thread-safe.
unsafe impl Sync for BackendHandles where FormulaLifetimeBackend: Send + Sync {}

impl BackendHandles {
    pub(super) fn new(handles: &FormulaLifetimeBackend) -> Self {
        let raw = std::ptr::from_ref(handles).cast_mut();
        // SAFETY: `raw` originates from a valid reference and is non-null.
        unsafe { Self(NonNull::new_unchecked(raw)) }
    }

    /// Accesses the underlying backend reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the backend remains alive (e.g., during
    /// active server callback execution before shutdown completion).
    pub(super) unsafe fn as_ref(&self) -> &FormulaLifetimeBackend {
        // SAFETY: guaranteed by caller contract.
        unsafe { self.0.as_ref() }
    }
}

pub(super) struct ServerBackends<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(super) handles: Option<BackendHandles>,
    pub(super) subscriptions: Option<AttachedSubscriptions<E, C>>,
}

/// A runtime and the server registered into that exact runtime travel together.
pub(super) struct AttachedSubscriptions<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    runtime: BackendSubscriptions<E, C>,
    server: BackendSubscriptionServer<E, C>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy for AttachedSubscriptions<E, C> {}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone for AttachedSubscriptions<E, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> AttachedSubscriptions<E, C> {
    #[cfg(all(test, feature = "handles"))]
    pub(super) fn is_runtime(&self, runtime: &SubscriptionRuntime<E, C>) -> bool {
        self.runtime.is_runtime(runtime)
    }

    /// # Safety
    /// The runtime must remain alive until this attachment has been withdrawn.
    unsafe fn register(
        runtime: BackendSubscriptions<E, C>,
        generation: ServerGeneration,
    ) -> XllResult<Self> {
        // SAFETY: the caller retains the runtime through attachment withdrawal.
        let server = unsafe { runtime.register_server(generation)? };
        Ok(Self { runtime, server })
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ServerBackends<E, C> {
    pub(super) fn subscription_server(&self) -> Option<BackendSubscriptionServer<E, C>> {
        self.subscriptions.as_ref().map(|attached| attached.server)
    }

    fn is_detached(&self) -> bool {
        self.handles.is_none() && self.subscriptions.is_none()
    }
}

/// Attaches a new update notifier to the subscription server backend.
///
/// # Safety
///
/// The caller must ensure that `server` (and its contained `operations` barrier
/// and pinned `callback`) outlives the update notifier attached to `subscription_server`.
pub(super) unsafe fn synchronize_callback_notification<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    server: &RtdServer<E, C>,
    callback: CallbackPtr,
) -> XllResult<()> {
    let subscription_server = server.backends.lock().subscription_server();
    let Some(subscription_server) = subscription_server else {
        return Ok(());
    };

    // SAFETY: caller guarantees `server` and its operations barrier outlive
    // the notifier attached to the subscription server.
    let notifier = unsafe { RtdNotifier::new(callback, NonNull::from(server.operations.as_ref())) };
    subscription_server.attach_update_notifier(notifier)?;
    Ok(())
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop for RtdServer<E, C> {
    fn drop(&mut self) {
        if !self.termination_worker.is_idle_or_joined() {
            // Dropping a JoinHandle would detach live code from the DLL that
            // owns it. Every removal path must join before releasing the final
            // ACTIVE_SERVER reference.
            std::process::abort();
        }
        let subscription_server = self.backends.lock().subscription_server();
        if let Some(subscription_server) = subscription_server {
            subscription_server.detach_update_notifier();
        }

        drain_callbacks(&self.callbacks);
    }
}

#[repr(C)]
pub(super) struct RtdServerVtable<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(super) query_interface:
        unsafe extern "system" fn(*mut RtdServer<E, C>, *const GUID, *mut *mut c_void) -> i32,
    pub(super) add_ref: unsafe extern "system" fn(*mut RtdServer<E, C>) -> u32,
    pub(super) release: unsafe extern "system" fn(*mut RtdServer<E, C>) -> u32,
    pub(super) get_type_info_count:
        unsafe extern "system" fn(*mut RtdServer<E, C>, *mut u32) -> i32,
    pub(super) get_type_info:
        unsafe extern "system" fn(*mut RtdServer<E, C>, u32, u32, *mut *mut c_void) -> i32,
    pub(super) get_ids_of_names: unsafe extern "system" fn(
        *mut RtdServer<E, C>,
        *const GUID,
        *const *const u16,
        u32,
        u32,
        *mut i32,
    ) -> i32,
    pub(super) invoke: unsafe extern "system" fn(
        *mut RtdServer<E, C>,
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
        unsafe extern "system" fn(*mut RtdServer<E, C>, *mut c_void, *mut i32) -> i32,
    pub(super) connect_data: unsafe extern "system" fn(
        *mut RtdServer<E, C>,
        i32,
        *mut *mut SAFEARRAY,
        *mut VARIANT_BOOL,
        *mut VARIANT,
    ) -> i32,
    pub(super) refresh_data:
        unsafe extern "system" fn(*mut RtdServer<E, C>, *mut i32, *mut *mut SAFEARRAY) -> i32,
    pub(super) disconnect_data: unsafe extern "system" fn(*mut RtdServer<E, C>, i32) -> i32,
    pub(super) heartbeat: unsafe extern "system" fn(*mut RtdServer<E, C>, *mut i32) -> i32,
    pub(super) server_terminate: unsafe extern "system" fn(*mut RtdServer<E, C>) -> i32,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdServer<E, C> {
    pub(super) const VTABLE: RtdServerVtable<E, C> = RtdServerVtable::<E, C> {
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
}
#[cfg(test)]
pub(super) fn discard_unpublished_server<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    pointer: usize,
    newly_created: bool,
) {
    if !newly_created {
        return;
    }

    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| {
                entry.pointer == pointer && entry.subscription_type == TypeId::of::<(E, C)>()
            })
            .map(|_| {
                let server = pointer as *mut RtdServer<E, C>;
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
    let access = match unsafe { (&*server).drain_access() } {
        Ok(termination) => termination,
        Err(_) => {
            // SAFETY: balance the temporary retained reference.
            unsafe { server_release(server) };
            return;
        }
    };
    let joined = access
        .join_worker()
        .unwrap_or_else(|_| std::process::abort());
    if joined.worker_panicked() {
        crate::diagnostics::report_no_unwind(
            "IRtdServer::deferred termination join",
            &XllError::Panic,
        );
    }

    let status = joined.teardown_and_remove_active(false);
    if status != S_OK {
        crate::diagnostics::report_no_unwind(
            "IRtdServer::unpublished server teardown",
            &XllError::WindowsApi {
                function: "IRtdServer::ServerTerminate",
                code: status,
            },
        );
    }
    drop(joined);
    // SAFETY: balance the temporary retained reference.
    unsafe { server_release(server) };
}

#[cfg(feature = "handles")]
pub(crate) fn shutdown<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    handles: &FormulaLifetimeBackend,
) -> XllResult<()> {
    let mut shutdown_error = None;
    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| entry.subscription_type == TypeId::of::<(E, C)>())
            .filter(|entry| {
                let server = entry.pointer as *mut RtdServer<E, C>;

                // SAFETY: ACTIVE_SERVER owns a live reference to this RtdServer<E, C>
                // while its mutex remains held, and handles backend is alive while attached.
                unsafe {
                    let backends = (*server).backends.lock();
                    // Deferred termination withdraws both backends before its
                    // worker exits. Lifecycle shutdown must still find and
                    // join that retained, detached ACTIVE_SERVER.
                    backends.is_detached()
                        || backends
                            .handles
                            .as_ref()
                            .is_some_and(|active| active.as_ref().identity() == handles.identity())
                }
            })
            .cloned()
            .inspect(|entry| {
                // SAFETY: ACTIVE_SERVER owns a live reference while its mutex
                // remains held. This temporary reference keeps the raw pointer
                // alive while shutdown waits outside the global mutex.
                unsafe { server_add_ref(entry.pointer as *mut RtdServer<E, C>) };
            })
    };

    if let Some(retained) = retained {
        let server = retained.pointer as *mut RtdServer<E, C>;

        // SAFETY: the temporary reference acquired above keeps `server` alive.
        let access = match unsafe { (&*server).drain_access() } {
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
        let joined = access
            .join_worker()
            .unwrap_or_else(|_| std::process::abort());
        if joined.worker_panicked() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("IRtdServer::deferred termination join", &error);
            shutdown_error = Some(error);
        }
        // A server can be shared with subscriptions. Stop both attached
        // backends and withdraw their non-owning pointers before releasing
        // ACTIVE_SERVER or the callback they may still reference.
        // The capability retains the server and excludes admitted operations.
        let status = joined.teardown_and_remove_active(false);
        if status != S_OK && shutdown_error.is_none() {
            shutdown_error = Some(XllError::WindowsApi {
                function: "IRtdServer::ServerTerminate",
                code: status,
            });
        }

        // Finish the phase transition while the temporary server reference
        // still guarantees that the borrowed barrier remains alive.
        drop(joined);

        // SAFETY: balance the temporary reference acquired while locating the
        // active server.
        unsafe { server_release(server) };
    }
    // Transport generations cover only observed topics. Prepared topics that
    // never reached claim_lifetime, and topics from a replaced server, are
    // still owned by this runtime and must be retired at this boundary.
    handles.terminate_all_topics();
    retry_git_revocation_debt();
    match shutdown_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn shutdown_subscriptions<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    subscriptions: &SubscriptionRuntime<E, C>,
) -> XllResult<()> {
    let mut shutdown_error = None;
    let retained = {
        let active = ACTIVE_SERVER.lock();
        active
            .as_ref()
            .filter(|entry| entry.subscription_type == TypeId::of::<(E, C)>())
            .filter(|entry| {
                let server = entry.pointer as *mut RtdServer<E, C>;

                // SAFETY: ACTIVE_SERVER owns a live reference to this RtdServer<E, C>
                // while its mutex remains held.
                unsafe {
                    let backends = (*server).backends.lock();
                    backends.is_detached()
                        || backends.subscriptions.as_ref().is_some_and(|active| {
                            active
                                .runtime
                                .same_runtime(BackendSubscriptions::new(subscriptions))
                        })
                }
            })
            .cloned()
            .inspect(|entry| {
                // SAFETY: ACTIVE_SERVER owns a live reference while its mutex
                // remains held. This temporary reference keeps the raw pointer
                // alive while shutdown waits outside the global mutex.
                unsafe { server_add_ref(entry.pointer as *mut RtdServer<E, C>) };
            })
    };

    if let Some(retained) = retained {
        let server = retained.pointer as *mut RtdServer<E, C>;

        // Stop all COM methods first. SubscriptionRuntime::close then closes
        // background publish/notification work that does not enter through COM.
        // SAFETY: the temporary reference acquired above keeps `server` alive.
        let access = match unsafe { (&*server).drain_access() } {
            Ok(termination) => termination,
            Err(_) => {
                // See the corresponding handle-shutdown branch above:
                // terminal unload cannot continue through a recursively
                // owned server operation.
                std::process::abort();
            }
        };
        let joined = access
            .join_worker()
            .unwrap_or_else(|_| std::process::abort());
        if joined.worker_panicked() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("IRtdServer::deferred termination join", &error);
            shutdown_error = Some(error);
        }
        let close_result = subscriptions.close();
        // Subscription producers stop before Addin::quiesce. Keep the handle
        // roots and their backend attached until shutdown(handles), which runs
        // after quiesce and layer disposal.
        // The capability retains the server and excludes admitted operations.
        let status = joined.teardown_and_remove_active(true);
        if status != S_OK && shutdown_error.is_none() {
            shutdown_error = Some(XllError::WindowsApi {
                function: "IRtdServer::ServerTerminate",
                code: status,
            });
        }

        // Finish the phase transition while the temporary server reference
        // still guarantees that the borrowed barrier remains alive.
        drop(joined);

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

#[cfg(feature = "handles")]
pub(super) fn ensure_server<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    handles: Option<&FormulaLifetimeBackend>,
    subscriptions: Option<BackendSubscriptions<E, C>>,
) -> XllResult<EnsuredServer<E, C>> {
    ensure_server_impl(handles, subscriptions)
}

pub(super) fn ensure_server_without_handles<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    subscriptions: Option<BackendSubscriptions<E, C>>,
) -> XllResult<EnsuredServer<E, C>> {
    ensure_server_impl(None, subscriptions)
}

fn ensure_server_impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    handles: Option<&FormulaLifetimeBackend>,
    subscriptions: Option<BackendSubscriptions<E, C>>,
) -> XllResult<EnsuredServer<E, C>> {
    #[cfg(test)]
    super::server_type::register_server_type::<E, C>()?;
    loop {
        let mut active = ACTIVE_SERVER.lock();
        if !super::server_type::is_registered::<E, C>() {
            return Err(XllError::Closing);
        }

        if let Some(existing) = active.as_ref() {
            if existing.subscription_type != TypeId::of::<(E, C)>() {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
                });
            }
            let server = existing.pointer as *mut RtdServer<E, C>;
            // Deferred ServerTerminate intentionally leaves ACTIVE_SERVER in place
            // until a host lifecycle path can join its coordinator. A ServerStart
            // failure after callback publication is also terminal for that
            // instance: Excel need not issue ServerTerminate after a failed start.
            // SAFETY: ACTIVE_SERVER owns a live server reference while locked.
            let operations = unsafe { (*server).operations.as_ref() };
            let phase = operations.phase();
            // SAFETY: the same global reference keeps the start-state atomic live.
            let failed_start =
                unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_START_FAILED;
            if phase != ServerPhase::Open || failed_start {
                // SAFETY: create a temporary reference that remains valid after the
                // global mutex is released for the potentially blocking join.
                unsafe { server_add_ref(server) };
                drop(active);

                // A failed ServerStart can still be leaving its admitted method.
                // Close and drain it before retiring the callback/backend state.
                // SAFETY: the temporary reference above keeps the barrier live.
                let access = match unsafe { (&*server).drain_access() } {
                    Ok(termination) => termination,
                    Err(_) => {
                        // SAFETY: balance the temporary reference above.
                        unsafe { server_release(server) };
                        return Err(XllError::Closing);
                    }
                };

                let joined = match access.join_worker() {
                    Ok(joined) => joined,
                    Err(_) => {
                        // SAFETY: balance the temporary reference above. A coordinator
                        // re-entering ensure_server must let an external thread reap it.
                        unsafe { server_release(server) };
                        return Err(XllError::Closing);
                    }
                };
                let worker_panicked = joined.worker_panicked();
                if worker_panicked {
                    crate::diagnostics::report_no_unwind(
                        "IRtdServer::deferred termination join",
                        &XllError::Panic,
                    );
                }

                // The resource-teardown guard serializes cleanup skipped by a
                // panicking worker, including secondary reapers whose
                // close_and_wait returned no termination owner.
                // SAFETY: the temporary reference keeps `server` live.
                let status = joined.teardown_and_remove_active(false);
                drop(joined);
                // SAFETY: balance the temporary reference acquired above.
                unsafe { server_release(server) };
                if status != S_OK {
                    return Err(XllError::WindowsApi {
                        function: "IRtdServer::ServerTerminate",
                        code: status,
                    });
                }
                if worker_panicked {
                    return Err(XllError::Panic);
                }
                continue;
            }

            // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
            // held. A closing server cannot accept a newly attached backend.
            let Some(_server_operation) = (unsafe { (*server).operations.enter() }) else {
                // ServerTerminate can close admission after the phase snapshot.
                // Retry retirement rather than exposing a transient transport
                // close as a runtime lifecycle failure.
                drop(active);
                continue;
            };

            // SAFETY: ACTIVE_SERVER owns a live server reference while its mutex is
            // held, so the RtdServer<E, C> and its `backends` mutex are valid.
            let mut backends = unsafe { (*server).backends.lock() };

            if let Some(handles) = handles {
                match backends.handles.as_ref() {
                    Some(active) => {
                        // SAFETY: active is attached to the server whose lock is held.
                        let active_ref = unsafe { active.as_ref() };
                        if active_ref.identity() != handles.identity() {
                            return Err(XllError::Internal {
                                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
                            });
                        }
                    }
                    None => {
                        backends.handles = Some(BackendHandles::new(handles));
                    }
                }
            }

            let (newly_attached_subscriptions, subscription_handle) =
                if let Some(subscriptions) = subscriptions {
                    match backends.subscriptions.as_ref() {
                        Some(active) if active.runtime.same_runtime(subscriptions) => {
                            (None, backends.subscription_server())
                        }
                        Some(_) => {
                            return Err(XllError::Internal {
                                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
                            });
                        }
                        None => {
                            // SAFETY: admission/shutdown retain the runtime through every typed handle call.
                            let attachment = unsafe {
                                AttachedSubscriptions::register(subscriptions, existing.generation)?
                            };
                            let handle = attachment.server;
                            backends.subscriptions = Some(attachment);
                            (Some(handle), Some(handle))
                        }
                    }
                } else {
                    (None, backends.subscription_server())
                };

            drop(backends);

            if let Some(handle) = newly_attached_subscriptions {
                // SAFETY: `server` was validated as non-null and COM keeps the server alive.
                let callback = unsafe { active_callback(&(*server).callbacks) };
                if let Some(callback) = callback {
                    // SAFETY: `callback` is pinned in `callbacks` and `operations`
                    // remains valid for the server lifetime.
                    let notifier = unsafe { RtdNotifier::new(callback, NonNull::from(operations)) };
                    handle.attach_update_notifier(notifier)?;
                }
            }

            // SAFETY: ACTIVE_SERVER owns a live server reference. This increments
            // the count to create the separate temporary reference held by the guard.
            unsafe { server_add_ref(server) };

            return Ok(EnsuredServer::<E, C> {
                active: existing.clone(),
                #[cfg(test)]
                newly_created: false,
                subscription_server: subscription_handle,
            });
        }

        let mut class_id = GUID::default();

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
                diagnostic_id:
                    crate::diagnostics::id::DiagnosticId::RTD_SERVER_GENERATION_EXHAUSTED,
            })?;
        let operations = ServerOperationBarrier::new().map_err(|error| XllError::WindowsApi {
            function: error.operation,
            code: error.code as i32,
        })?;

        let attached_subscriptions = if let Some(subscriptions) = subscriptions {
            // SAFETY: `subscriptions` is the active generation's subscription runtime which
            // outlives the RTD server lifecycle (protected by COM refcounting and shutdown barriers).
            Some(unsafe { AttachedSubscriptions::register(subscriptions, generation)? })
        } else {
            None
        };

        let subscription_handle = attached_subscriptions
            .as_ref()
            .map(|attached| attached.server);
        let server = Box::new(RtdServer::<E, C> {
            vtable: &RtdServer::<E, C>::VTABLE,
            references: AtomicU32::new(1),
            start_state: AtomicU8::new(SERVER_NOT_STARTED),
            observations: Mutex::new(ObservationState::default()),
            generation,
            operations: Arc::new(operations),
            termination_worker: TerminationWorker::default(),
            backends: Mutex::new(ServerBackends::<E, C> {
                handles: handles.map(BackendHandles::new),
                subscriptions: attached_subscriptions,
            }),
            callbacks: Mutex::new(ServerCallbacks::default()),
            _module_lease: ComObjectLease::new(ComObjectKind::Server),
        });

        let pointer = Box::into_non_null(server).as_ptr() as usize;
        let entry = ActiveServer {
            class_id,
            prog_id: format!("XlFnRtd_{}", guid_compact(class_id)),
            pointer,
            generation,
            subscription_type: TypeId::of::<(E, C)>(),
        };

        *active = Some(entry.clone());

        // SAFETY: the construction reference is now owned by ACTIVE_SERVER.
        // Incrementing here creates a separate temporary reference for EnsuredServer<E, C>.
        unsafe { server_add_ref(pointer as *mut RtdServer<E, C>) };

        return Ok(EnsuredServer::<E, C> {
            active: entry,
            #[cfg(test)]
            newly_created: true,
            subscription_server: subscription_handle,
        });
    }
}
pub(super) unsafe extern "system" fn server_query_interface<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    com_lifetime_boundary("IRtdServer::QueryInterface", E_UNEXPECTED, || {
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
    })
}

pub(super) unsafe extern "system" fn server_add_ref<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
) -> u32 {
    com_lifetime_boundary("IRtdServer::AddRef", 0, || {
        // SAFETY: COM and internal callers invoke AddRef only on a live RtdServer<E, C>.
        let references = unsafe { &(*this).references };
        references
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
            + 1
    })
}

pub(super) unsafe extern "system" fn server_release<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
) -> u32 {
    com_lifetime_boundary("IRtdServer::Release", 0, || {
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
            // allocation originally produced by Box::into_non_null.
            drop(unsafe { Box::from_non_null(this) });
        }

        remaining
    })
}

unsafe extern "system" fn server_get_type_info_count<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    count: *mut u32,
) -> i32 {
    com_lifetime_boundary("IDispatch::GetTypeInfoCount", E_UNEXPECTED, || {
        if this.is_null() || count.is_null() {
            return E_POINTER;
        }

        // SAFETY: `count` was validated as non-null and is a writable COM output.
        unsafe { *count = 0 };

        S_OK
    })
}

unsafe extern "system" fn server_get_type_info<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    index: u32,
    _locale: u32,
    output: *mut *mut c_void,
) -> i32 {
    com_lifetime_boundary("IDispatch::GetTypeInfo", E_UNEXPECTED, || {
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
    })
}

pub(super) unsafe extern "system" fn server_start<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    callback: *mut c_void,
    result: *mut i32,
) -> i32 {
    com_boundary("IRtdServer::ServerStart", || {
        // SAFETY: the arguments are forwarded from the COM vtable call. The
        // inner function validates each pointer before dereferencing it.
        unsafe { server_start_inner(this, callback, result) }
    })
}

unsafe fn server_start_inner<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
    callback: *mut c_void,
    result: *mut i32,
) -> i32 {
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
    let Some(start_reservation) = unsafe { &*this }.reserve_start() else {
        return E_FAIL;
    };

    // SAFETY: `this` was validated as non-null and COM keeps the server alive
    // for the duration of ServerStart.
    let _subscription_server = unsafe { (*this).backends.lock().subscription_server() };

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

    // SAFETY: the retained COM server owns the independently allocated barrier.
    let operations = unsafe { Arc::clone(&(*this).operations) };
    let callback = match RetainedUpdateCallback::new(cookie, operations) {
        Ok(callback) => Box::new(callback),
        Err(error) => {
            crate::diagnostics::report_no_unwind(
                "IRtdServer::ServerStart notification worker",
                &error,
            );
            return E_FAIL;
        }
    };

    let (start_reservation, callback_ptr) = start_reservation.publish_callback(callback);

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

pub(super) unsafe extern "system" fn connect_data<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
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

unsafe fn connect_data_inner<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
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

    let (handles, subscriptions) = {
        // SAFETY: `this` was validated as non-null and COM retains the server during ConnectData.
        let backends = unsafe { (*this).backends.lock() };
        (backends.handles, backends.subscriptions)
    };

    // SAFETY: `this` remains valid for the duration of ConnectData.
    let generation = unsafe { (*this).generation };

    let status = if let Some(rtd_key) = key.strip_prefix("handle:") {
        let Some(handles) = handles.as_ref() else {
            return E_FAIL;
        };

        // SAFETY: Excel RTD server outlives or is bounded by the lifecycle coordinator
        // which withdraws/shuts down the server before dropping the backend.
        let handles_ref = unsafe { handles.as_ref() };
        match handles_ref.connect_transaction(lifetime_generation(generation), topic_id, rtd_key) {
            // SAFETY: result is writable COM storage. The concrete handle
            // transaction stays owned through BSTR conversion and commit.
            Ok(connection) => unsafe {
                finish_connect_data(
                    connection,
                    result,
                    |connection, result| write_bstr_variant(result, connection.token()),
                    |connection| connection.commit(),
                )
            },
            Err(error) => {
                crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
                return E_FAIL;
            }
        }
    } else if let Ok(sub_key) = crate::subscription::SubscriptionKey::parse_transport(&key) {
        let Some(subscriptions) = subscriptions.as_ref() else {
            return E_FAIL;
        };
        // SAFETY: COM admission retains this adapter and runtime. It resolves
        // the key and completes its typed connection before returning.
        unsafe {
            subscriptions.server.connect_data(
                crate::subscription::TopicId(topic_id),
                sub_key,
                result,
            )
        }
    } else {
        return E_INVALIDARG;
    };

    if status != S_OK {
        return status;
    }

    // SAFETY: `new_values` was validated as non-null and remains writable for
    // the duration of this method.
    unsafe { *new_values = VARIANT_TRUE };
    S_OK
}

pub(super) unsafe extern "system" fn refresh_data<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    topic_count: *mut i32,
    result: *mut *mut SAFEARRAY,
) -> i32 {
    com_boundary("IRtdServer::RefreshData", || {
        // SAFETY: arguments are forwarded from the COM vtable invocation. The
        // inner function validates all pointers before dereferencing them.
        unsafe { refresh_data_inner(this, topic_count, result) }
    })
}

unsafe fn refresh_data_inner<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
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
    let subscription_server = unsafe { (*this).backends.lock().subscription_server() };

    // Snapshot the notice before fetching data. Completion must not clear a
    // newer notice queued by publication/refresh rearming in the meantime.
    // SAFETY: this admitted operation keeps all callback records live.
    let notice = unsafe { active_callback(&(*this).callbacks) }.map(|callback| {
        // SAFETY: callback records are retained until COM operations drain.
        let sequence = unsafe { callback.0.as_ref() }.pending_notice();
        (callback, sequence)
    });

    let Some(subscription_server) = subscription_server else {
        // SAFETY: `topic_count` and `result` are valid COM output parameters.
        let status = unsafe { write_refresh_data(topic_count, result, &[]) };
        if status == S_OK
            && let Some((callback, sequence)) = notice
        {
            // SAFETY: the admitted COM operation retains the callback record.
            unsafe { callback.0.as_ref() }.acknowledge(sequence);
        }
        return status;
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

    match batch.complete(outcome) {
        Err(error) => {
            crate::diagnostics::report_no_unwind("IRtdServer::RefreshData rearm", &error);
        }
        Ok(()) if status == S_OK => {
            if let Some((callback, sequence)) = notice {
                // SAFETY: the admitted COM operation retains the callback record.
                unsafe { callback.0.as_ref() }.acknowledge(sequence);
            }
        }
        Ok(()) => {}
    }

    status
}

pub(super) unsafe extern "system" fn disconnect_data<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    topic_id: i32,
) -> i32 {
    com_boundary("IRtdServer::DisconnectData", || {
        // SAFETY: arguments are forwarded from the COM vtable invocation. The
        // inner function validates `this` before dereferencing it.
        unsafe { disconnect_data_inner(this, topic_id) }
    })
}

unsafe fn disconnect_data_inner<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
    topic_id: i32,
) -> i32 {
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
        (backends.handles, backends.subscription_server())
    };

    // SAFETY: `this` remains valid for the duration of DisconnectData.
    let generation = unsafe { (*this).generation };

    if let Some(subscription_server) = subscription_server.as_ref() {
        // DisconnectData can run while this STA is servicing UpdateNotify,
        // or while a publisher/producer is waiting for an STA COM call. Do not
        // wait for either worker here. Revoke its topic now and transfer the
        // final sink-user barrier to a generation-owned cleanup worker.
        // SAFETY: COM owns a live server reference throughout this method.
        let reference = unsafe { OwnedServerReference::acquire(this) };
        // SAFETY: the lease retains `reference` until after this guard drops.
        let call = match unsafe { (*this).operations.enter_owned() } {
            Some(call) => call,
            None => return E_FAIL,
        };
        let lease = DeferredDisconnectLease {
            _call: call,
            _reference: reference,
        };
        match subscription_server.disconnect_deferred(crate::subscription::TopicId(topic_id), lease)
        {
            Err(error) if !matches!(error, crate::XllError::Closing) => {
                crate::diagnostics::report_no_unwind("IRtdServer::DisconnectData", &error);
                return E_FAIL;
            }
            _ => {}
        }
    }

    if let Some(handles) = handles {
        // SAFETY: Excel RTD server outlives or is bounded by the lifecycle coordinator
        // which withdraws/shuts down the server before dropping the backend.
        unsafe { handles.as_ref() }.disconnect(lifetime_generation(generation), topic_id);
    }

    S_OK
}

pub(super) unsafe extern "system" fn heartbeat<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
    result: *mut i32,
) -> i32 {
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
            let subscription_server = unsafe { (*this).backends.lock().subscription_server() };
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

pub(super) unsafe extern "system" fn server_terminate<
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    this: *mut RtdServer<E, C>,
) -> i32 {
    com_boundary("IRtdServer::ServerTerminate", || {
        // SAFETY: `this` is forwarded from the COM vtable invocation. The inner
        // function validates it before dereferencing.
        unsafe { server_terminate_inner(this) }
    })
}

unsafe fn server_terminate_inner<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
) -> i32 {
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

    // SAFETY: COM retains this server through the complete terminal request.
    unsafe { finish_termination_request(this, request) }
}

unsafe fn finish_termination_request<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    this: *mut RtdServer<E, C>,
    request: ServerTerminationRequest<'_>,
) -> i32 {
    match request {
        ServerTerminationRequest::Complete | ServerTerminationRequest::InProgress => S_OK,
        ServerTerminationRequest::Synchronous(termination) => {
            // SAFETY: COM retains `this` through this synchronous request.
            let access = QuiescentServerAccess::from_termination(unsafe { &*this }, termination);
            let joined = match access.join_worker() {
                Ok(joined) => joined,
                Err(_) => return E_FAIL,
            };
            joined.teardown_and_remove_active(false)
        }
        ServerTerminationRequest::Deferred(reservation) => {
            // The consuming startup owner holds the gate lock through spawn,
            // handle registration and commit. No detached ticket can be lost
            // before a separate caller commits the termination phase.
            // SAFETY: COM retains this server through the complete request.
            let start = match reservation.start(unsafe { &*this }) {
                Ok(start) => start,
                Err(_) => return E_FAIL,
            };
            match start.spawn() {
                Ok(()) => S_OK,
                Err(_) => E_FAIL,
            }
        }
    }
}

/// The worker receives the server lifetime and its exact completion ticket as
/// one private payload; it cannot substitute a target while waiting.
pub(super) struct DeferredTerminationWork<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> {
    reference: OwnedServerReference,
    ticket: DeferredTerminationTicket,
    _catalog: std::marker::PhantomData<fn() -> (E, C)>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdServer<E, C> {
    pub(super) fn deferred_termination_work(
        &self,
        ticket: DeferredTerminationTicket,
    ) -> DeferredTerminationWork<E, C> {
        // This constructor is used only by the gate's startup transaction,
        // which verifies reservation ownership before issuing the ticket.
        if !ticket.belongs_to(self.operations.as_ref()) {
            xlfn_kernel::invariant::fail_stop();
        }
        // SAFETY: the borrowed server remains retained by the requesting COM
        // call. The new reference covers its one fixed deferred worker.
        let reference =
            unsafe { OwnedServerReference::acquire(std::ptr::from_ref(self).cast_mut()) };
        DeferredTerminationWork {
            reference,
            ticket,
            _catalog: std::marker::PhantomData,
        }
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> DeferredTerminationWork<E, C> {
    pub(super) fn spawn(self) -> std::io::Result<std::thread::JoinHandle<()>> {
        spawn_deferred_termination(self)
    }
}

fn spawn_deferred_termination<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    work: DeferredTerminationWork<E, C>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    #[cfg(test)]
    if FAIL_DEFERRED_TERMINATION_SPAWN.swap(false, Ordering::AcqRel) {
        return Err(std::io::Error::other(
            "injected deferred RTD termination spawn failure",
        ));
    }

    std::thread::Builder::new()
        .name("xlfn-rtd-termination".to_owned())
        .spawn(move || deferred_termination_worker(work))
}

fn deferred_termination_worker<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    work: DeferredTerminationWork<E, C>,
) {
    let DeferredTerminationWork {
        reference, ticket, ..
    } = work;
    let this = reference.pointer as *mut RtdServer<E, C>;
    // Keep the internal reference outside the unwind boundary. It remains live
    // until all cleanup and phase signaling have completed.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let result = ticket.with_completion(|termination| {
            #[cfg(test)]
            if PANIC_DEFERRED_TERMINATION_CLEANUP.swap(false, Ordering::AcqRel) {
                panic!("injected deferred RTD cleanup panic");
            }

            // Deferred cleanup intentionally retains ACTIVE_SERVER. The next
            // xlAutoRemove or ensure_server joins this worker before removing that
            // final module-lifetime reference.
            // SAFETY: the worker's reference retains this exact server.
            let access = QuiescentServerAccess::from_termination(unsafe { &*this }, termination);
            let status = access.teardown_resources();
            if status != S_OK {
                crate::diagnostics::report_no_unwind(
                    "IRtdServer::deferred termination",
                    &XllError::WindowsApi {
                        function: "IRtdServer::ServerTerminate",
                        code: status,
                    },
                );
            }
            drop(access);
        });
        if let Err(error) = result {
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
            // Reopening after a committed spawn would strand a completed
            // worker handle on a reusable server. A live event cannot fail.
            std::process::abort();
        }
    }));

    if let Err(payload) = outcome {
        crate::diagnostics::report_no_unwind("IRtdServer::deferred termination", &XllError::Panic);
        drop(reference);
        std::panic::resume_unwind(payload);
    }
    drop(reference);
}

/// Authority to dispose resources on one retained, drained COM server.
/// The owned reference outlives both the drained proof and any teardown guard.
pub(super) struct QuiescentServerAccess<
    'server,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    server: &'server RtdServer<E, C>,
    drained: DrainedServerOperations<'server>,
    _reference: OwnedServerReference,
}

/// The deferred coordinator is absent or joined, so the active reference may go.
pub(super) struct JoinedServerAccess<
    'server,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    access: QuiescentServerAccess<'server, E, C>,
    worker_panicked: bool,
}

struct DisposedServerResources<'access> {
    _exclusive: ServerResourceTeardown<'access>,
    keep_active: bool,
    status: i32,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdServer<E, C> {
    pub(super) fn drain_access(&self) -> Result<QuiescentServerAccess<'_, E, C>, ServerCloseError> {
        // SAFETY: this borrowed server is retained by the caller. Take an
        // independent lease before drain can pump arbitrary COM callbacks.
        let reference =
            unsafe { OwnedServerReference::acquire(std::ptr::from_ref(self).cast_mut()) };
        let drained = self.operations.drain()?;
        Ok(QuiescentServerAccess {
            server: self,
            drained,
            _reference: reference,
        })
    }
}

impl<'server, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    QuiescentServerAccess<'server, E, C>
{
    fn from_termination(
        server: &'server RtdServer<E, C>,
        termination: ServerTermination<'server>,
    ) -> Self {
        let drained = termination.into_drained();
        // A lifetime alone cannot establish instance identity. This boundary
        // also handles guards received from ServerTerminationRequest.
        if !drained.belongs_to(server.operations.as_ref()) {
            xlfn_kernel::invariant::fail_stop();
        }
        // SAFETY: the borrowed server is retained by this call's COM owner.
        let reference =
            unsafe { OwnedServerReference::acquire(std::ptr::from_ref(server).cast_mut()) };
        Self {
            server,
            drained,
            _reference: reference,
        }
    }

    pub(super) fn join_worker(self) -> Result<JoinedServerAccess<'server, E, C>, ServerCloseError> {
        let worker_panicked = match self.server.termination_worker.seal_and_join() {
            Ok(()) => false,
            // join has consumed the thread even when its result was a panic.
            Err(ServerCloseError::WorkerPanicked) => true,
            Err(error) => return Err(error),
        };
        Ok(JoinedServerAccess {
            access: self,
            worker_panicked,
        })
    }

    /// Deferred workers may dispose resources but cannot release ACTIVE_SERVER.
    pub(super) fn teardown_resources(&self) -> i32 {
        match self.dispose_resources(false) {
            Ok(disposed) => disposed.status,
            Err(status) => status,
        }
    }

    fn dispose_resources(
        &self,
        retain_handle_topics: bool,
    ) -> Result<DisposedServerResources<'_>, i32> {
        // Drain and exclusive disposal are separate authorities: another reaper
        // may already have published Terminated while still holding resources.
        let exclusive = self.drained.teardown_resources().map_err(|_| E_FAIL)?;
        Ok(self.dispose_resources_with_guard(exclusive, retain_handle_topics))
    }

    fn dispose_resources_with_guard<'access>(
        &'access self,
        exclusive: ServerResourceTeardown<'access>,
        retain_handle_topics: bool,
    ) -> DisposedServerResources<'access> {
        let server = self.server;
        let (handles, subscription_server, keep_active) = {
            let backends = server.backends.lock();
            (
                if retain_handle_topics {
                    None
                } else {
                    backends.handles
                },
                backends.subscription_server(),
                retain_handle_topics && backends.handles.is_some(),
            )
        };
        let status = match subscription_server.as_ref() {
            Some(subscription_server) => match subscription_server.terminate() {
                Ok(()) => S_OK,
                Err(_) => E_FAIL,
            },
            None => S_OK,
        };
        if let Some(handles) = handles {
            // SAFETY: attached backends stay published through their last use;
            // generation shutdown waits for this exclusive disposal to finish.
            unsafe { handles.as_ref() }.terminate_topics(lifetime_generation(server.generation));
        }
        {
            let mut backends = server.backends.lock();
            if !retain_handle_topics {
                backends.handles = None;
            }
            backends.subscriptions = None;
        }
        drain_callbacks(&server.callbacks);
        DisposedServerResources {
            _exclusive: exclusive,
            keep_active,
            status,
        }
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> JoinedServerAccess<'_, E, C> {
    pub(super) fn worker_panicked(&self) -> bool {
        self.worker_panicked
    }

    pub(super) fn teardown_and_remove_active(&self, retain_handle_topics: bool) -> i32 {
        let exclusive = match self.access.drained.teardown_resources() {
            Ok(exclusive) => exclusive,
            Err(_) => return E_FAIL,
        };
        let this = std::ptr::from_ref(self.access.server).cast_mut();
        let keep_active =
            retain_handle_topics && self.access.server.backends.lock().handles.is_some();
        if !keep_active
            && !ACTIVE_SERVER
                .lock()
                .as_ref()
                .is_some_and(|entry| entry.pointer == this as usize)
        {
            // A previous reaper completed disposal before removing this target.
            // Preserve the terminal no-op without retrying unrelated GIT debt.
            return S_OK;
        }
        let disposed = self
            .access
            .dispose_resources_with_guard(exclusive, retain_handle_topics);
        if !disposed.keep_active {
            let owned = {
                let mut active = ACTIVE_SERVER.lock();
                active
                    .as_ref()
                    .is_some_and(|entry| entry.pointer == this as usize)
                    .then(|| active.take())
                    .flatten()
                    .is_some()
            };
            if owned {
                // SAFETY: our COM lease keeps the target and its exclusive
                // guard live after removing the global reference. The joined
                // capability proves no deferred coordinator remains in code.
                unsafe { server_release(this) };
            }
        }
        disposed.status
    }
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
