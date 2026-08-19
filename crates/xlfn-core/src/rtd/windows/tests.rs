use super::*;

use crate::subscription::{
    RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdUpdate, StoredRtdValue,
};
use std::marker::PhantomData;
use std::ptr;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::win32::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, COINIT_MULTITHREADED, CoInitializeEx,
    CoUninitialize, DISP_E_BADPARAMCOUNT, DISP_E_MEMBERNOTFOUND, DISP_E_TYPEMISMATCH,
    DISP_E_UNKNOWNNAME, DISPATCH_METHOD, DISPID_UNKNOWN, RPC_E_CHANGED_MODE, S_FALSE, S_OK,
    SAFEARRAYBOUND, SafeArrayCreate, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement,
    SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayPutElement, SysAllocStringLen, SysStringLen,
    VT_ARRAY, VT_BOOL, VT_BSTR, VT_BYREF, VT_EMPTY, VT_ERROR, VT_I4, VT_R8, VT_VARIANT,
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
            let attached =
                ensure_server(None, Some(&attached_subscriptions)).expect("attach subscriptions");
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
    TestClassFactory(NonNull::new(output.cast()).expect("DllGetClassObject returned null factory"))
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
                diagnostic_id: crate::DiagnosticId::RTD_DISPATCH,
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
                StoredRtdValue::Number(expected) => {
                    assert_eq!(value.Anonymous.Anonymous.vt, VT_R8);
                    assert_eq!(value.Anonymous.Anonymous.Anonymous.dblVal, *expected);
                }
                StoredRtdValue::Integer(expected) => {
                    assert_eq!(value.Anonymous.Anonymous.vt, VT_I4);
                    assert_eq!(value.Anonymous.Anonymous.Anonymous.lVal, *expected);
                }
                StoredRtdValue::Boolean(expected) => {
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
                StoredRtdValue::String(expected) => {
                    assert_eq!(value.Anonymous.Anonymous.vt, VT_BSTR);
                    let bstr = value.Anonymous.Anonymous.Anonymous.bstrVal;
                    assert!(!bstr.is_null());
                    let length = SysStringLen(bstr) as usize;
                    let actual = String::from_utf16_lossy(std::slice::from_raw_parts(bstr, length));
                    assert_eq!(actual, expected.as_str());
                }
                StoredRtdValue::Error(expected) => {
                    assert_eq!(value.Anonymous.Anonymous.vt, VT_ERROR);
                    assert_eq!(
                        value.Anonymous.Anonymous.Anonymous.scode,
                        2000 + expected.0.code()
                    );
                }
                StoredRtdValue::Empty => {
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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
            unsafe { (unknown_vtable.QueryInterface)(server_unknown.as_ptr(), &iid, &mut queried) },
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
    let ensured = ensure_server(Some(&handles), None).unwrap();

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
    let ensured = ensure_server(Some(&handles), None).unwrap();
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
    let ensured = ensure_server(Some(&handles), None).unwrap();

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
            ((*vtable).get_ids_of_names)(server.as_ptr(), &null_iid, names.as_ptr(), 1, 0, &mut id)
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
    let ensured = ensure_server(Some(&handles), None).unwrap();

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
    let ensured = ensure_server(None, Some(&subscriptions)).unwrap();
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
    assert_eq!(conn.value(), &StoredRtdValue::Number(12.5));
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
    let _active = ensure_server(Some(&handles), None).unwrap();
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

    let first = ensure_server(Some(&handles), None).unwrap();
    assert!(first.newly_created);

    let second = ensure_server(None, Some(&subscriptions)).unwrap();
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

    let ensured = ensure_server(None, Some(&subscriptions)).unwrap();
    let server = ensured.active.pointer as *mut RtdServer;

    let callback = Arc::new(RetainedUpdateCallback {
        cookie: None,
        drop_hook: None,
    });
    // SAFETY: EnsuredServer keeps server reference alive
    unsafe {
        install_callback(&(*server).callbacks, callback);
    }

    let notifier_state = Arc::new(crate::rtd::test_support::TestNotifierState::new());
    let handle = ensured.subscription_server.as_ref().unwrap();
    handle
        .attach_update_notifier(crate::rtd::RtdNotifier::for_test(Arc::clone(
            &notifier_state,
        )))
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

    assert_eq!(notifier_state.calls.load(Ordering::SeqCst), 1);

    for _ in 0..100 {
        let _res = ensure_server(None, Some(&subscriptions)).unwrap();
    }

    assert_eq!(notifier_state.calls.load(Ordering::SeqCst), 1);
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

#[test]
fn unwrap_dispatch_variant_enforces_single_level_indirection() {
    // 1. Direct VARIANT -> returns argument pointer directly
    let mut direct = VARIANT::default();
    direct.Anonymous.Anonymous.vt = VT_I4;
    direct.Anonymous.Anonymous.Anonymous.lVal = 42;

    // SAFETY: `direct` is a readable VARIANT on the stack.
    let unwrapped = unsafe { unwrap_dispatch_variant(&mut direct) };
    assert_eq!(unwrapped.map(|p| p.as_ptr()), Some(&mut direct as *mut _));

    // 2. VT_BYREF | VT_VARIANT -> VT_I4 -> returns inner VARIANT pointer
    let mut inner = VARIANT::default();
    inner.Anonymous.Anonymous.vt = VT_I4;
    inner.Anonymous.Anonymous.Anonymous.lVal = 42;

    let mut byref_valid = VARIANT::default();
    byref_valid.Anonymous.Anonymous.vt = VT_BYREF | VT_VARIANT;
    byref_valid.Anonymous.Anonymous.Anonymous.pvarVal = &mut inner;

    // SAFETY: both VARIANTs are readable on the stack.
    let unwrapped = unsafe { unwrap_dispatch_variant(&mut byref_valid) };
    assert_eq!(unwrapped.map(|p| p.as_ptr()), Some(&mut inner as *mut _));

    // 3. VT_BYREF | VT_VARIANT -> null -> returns None
    let mut byref_null = VARIANT::default();
    byref_null.Anonymous.Anonymous.vt = VT_BYREF | VT_VARIANT;
    byref_null.Anonymous.Anonymous.Anonymous.pvarVal = ptr::null_mut();

    // SAFETY: `byref_null` is a readable VARIANT on the stack.
    let unwrapped = unsafe { unwrap_dispatch_variant(&mut byref_null) };
    assert_eq!(unwrapped, None);

    // 4. VT_BYREF | VT_VARIANT -> VT_BYREF | VT_VARIANT -> returns None (automation spec violation)
    let mut nested_inner = VARIANT::default();
    nested_inner.Anonymous.Anonymous.vt = VT_BYREF | VT_VARIANT;
    nested_inner.Anonymous.Anonymous.Anonymous.pvarVal = &mut inner;

    let mut byref_nested = VARIANT::default();
    byref_nested.Anonymous.Anonymous.vt = VT_BYREF | VT_VARIANT;
    byref_nested.Anonymous.Anonymous.Anonymous.pvarVal = &mut nested_inner;

    // SAFETY: all VARIANTs are readable on the stack.
    let unwrapped = unsafe { unwrap_dispatch_variant(&mut byref_nested) };
    assert_eq!(unwrapped, None);
}
