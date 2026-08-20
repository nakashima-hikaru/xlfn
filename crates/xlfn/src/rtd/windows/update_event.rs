use super::IID_IRTD_UPDATE_EVENT;
use super::global_interface_table::get_git;
use super::module_state::COM_MODULE_LIFETIME;
use super::server_gate::ServerOperationBarrier;
use crate::win32::{COINIT_MULTITHREADED, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct ServerCallbacks {
    active: Option<Arc<RetainedUpdateCallback>>,
    retired: Vec<Arc<RetainedUpdateCallback>>,
}

struct ComApartmentGuard {
    should_uninit: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl ComApartmentGuard {
    fn enter() -> Result<Self, i32> {
        use crate::win32::CoInitializeEx;

        // SAFETY: the reserved pointer is null as required by CoInitializeEx.
        let status = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED as u32) };

        match status {
            S_OK | S_FALSE => Ok(Self {
                should_uninit: true,
                _not_send_or_sync: PhantomData,
            }),
            RPC_E_CHANGED_MODE => Ok(Self {
                // The thread already owns a different COM apartment. This
                // call did not add an initialization reference to balance.
                should_uninit: false,
                _not_send_or_sync: PhantomData,
            }),
            error => Err(error),
        }
    }
}

impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            use crate::win32::CoUninitialize;

            // SAFETY: `should_uninit` is set only after a successful
            // CoInitializeEx call, and this guard performs exactly one matching
            // CoUninitialize call while being used on the initializing thread.
            unsafe { CoUninitialize() };
        }
    }
}

pub(super) struct GitCookieLease {
    cookie: Option<std::num::NonZeroU32>,
}

impl GitCookieLease {
    pub(super) fn from_registered(cookie: std::num::NonZeroU32) -> Self {
        COM_MODULE_LIFETIME.git_cookie_registered();
        Self {
            cookie: Some(cookie),
        }
    }

    fn raw(&self) -> u32 {
        self.cookie
            .expect("live GIT cookie lease contains a cookie")
            .get()
    }
}

impl Drop for GitCookieLease {
    fn drop(&mut self) {
        let Some(cookie) = self.cookie.take() else {
            return;
        };

        match revoke_git_cookie(cookie.get()) {
            Ok(()) => COM_MODULE_LIFETIME.git_cookie_revoked(),
            Err(error) => {
                COM_MODULE_LIFETIME.git_cookie_revocation_deferred(cookie);
                crate::diagnostics::report_no_unwind("RTD GIT callback revocation", &error);
            }
        }
    }
}

pub(super) struct RetainedUpdateCallback {
    pub(super) cookie: Option<GitCookieLease>,
    #[cfg(test)]
    pub(super) drop_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn revoke_git_cookie(cookie: u32) -> XllResult<()> {
    if cookie == 0 {
        return Ok(());
    }

    let _apartment = ComApartmentGuard::enter().map_err(|code| XllError::ExcelApi {
        function: "CoInitializeEx",
        code,
    })?;
    // SAFETY: this thread has entered a COM apartment. `get_git` returns one
    // owned GIT wrapper. Its COM reference is released exactly once after the
    // synchronous revocation attempt.
    unsafe {
        let git = get_git().map_err(|status| XllError::ExcelApi {
            function: "CoCreateInstance(IGlobalInterfaceTable)",
            code: status,
        })?;
        let status = git.revoke(cookie);
        if status >= 0 {
            Ok(())
        } else {
            Err(XllError::ExcelApi {
                function: "IGlobalInterfaceTable::RevokeInterfaceFromGlobal",
                code: status,
            })
        }
    }
}

pub(super) fn retry_git_revocation_debt() {
    retry_git_revocation_debt_with(revoke_git_cookie);
}

pub(super) fn retry_git_revocation_debt_with(mut revoke: impl FnMut(u32) -> XllResult<()>) {
    let claims = COM_MODULE_LIFETIME.claim_git_revocation_debt_batch();
    for claim in claims {
        match revoke(claim.raw()) {
            Ok(()) => claim.resolve(),
            Err(error) => {
                crate::diagnostics::report_no_unwind("RTD GIT callback revocation retry", &error);
                // The unresolved claim is requeued by Drop.
            }
        }
    }
}

impl RetainedUpdateCallback {
    fn notify(&self) -> XllResult<()> {
        let Some(cookie) = self.cookie.as_ref() else {
            return Ok(());
        };
        let cookie = cookie.raw();
        let _apartment = ComApartmentGuard::enter().map_err(|code| XllError::ExcelApi {
            function: "CoInitializeEx",
            code,
        })?;

        // SAFETY: this thread has entered a COM apartment. `get_git` returns
        // one live IGlobalInterfaceTable wrapper on success, and the wrapper
        // validates the GIT output before returning its owned reference.
        unsafe {
            let git = get_git().map_err(|_| XllError::Internal {
                diagnostic_id: crate::DiagnosticId::GIT_NULL,
            })?;
            let proxy = git
                .get_interface(cookie, &IID_IRTD_UPDATE_EVENT)
                .map_err(|code| XllError::ExcelApi {
                    function: "IGlobalInterfaceTable::GetInterfaceFromGlobal",
                    code,
                })?;
            let event = OwnedRtdUpdateEvent::from_raw(proxy.cast());
            let notify_status = event.notify();

            if notify_status != S_OK {
                return Err(XllError::ExcelApi {
                    function: "IRTDUpdateEvent::UpdateNotify",
                    code: notify_status,
                });
            }
        }

        Ok(())
    }
}

impl Drop for RetainedUpdateCallback {
    fn drop(&mut self) {
        // RevokeInterfaceFromGlobal must run before the test hook, matching
        // the historical callback-drop ordering explicitly.
        drop(self.cookie.take());

        #[cfg(test)]
        if let Some(drop_hook) = self.drop_hook.as_ref() {
            drop_hook();
        }
    }
}

pub(super) fn install_callback(
    callbacks: &Mutex<ServerCallbacks>,
    callback: Arc<RetainedUpdateCallback>,
) {
    let mut callbacks = callbacks.lock();
    if let Some(previous) = callbacks.active.replace(callback) {
        // Revoking a GIT cookie can synchronously release arbitrary COM code.
        // Keep replaced callbacks alive until the server operation barrier has
        // reached quiescence instead of dropping one during ServerStart.
        callbacks.retired.push(previous);
    }
}

pub(super) fn active_callback(
    callbacks: &Mutex<ServerCallbacks>,
) -> Option<Arc<RetainedUpdateCallback>> {
    callbacks.lock().active.clone()
}

pub(super) fn drain_callbacks(callbacks: &Mutex<ServerCallbacks>) {
    let retired = {
        let mut callbacks = callbacks.lock();
        let mut retired = std::mem::take(&mut callbacks.retired);
        retired.extend(callbacks.active.take());
        retired
    };

    // RetainedUpdateCallback::drop revokes a GIT cookie and can therefore run
    // external, reentrant COM code. The callback mutex must not be held here.
    drop(retired);
    retry_git_revocation_debt();
}

#[cfg(not(test))]
#[derive(Clone)]
pub(crate) struct RtdNotifier {
    inner: Arc<RtdNotifierInner>,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RtdNotifier {
    inner: RtdNotifierKind,
}

#[cfg(test)]
#[derive(Clone)]
enum RtdNotifierKind {
    Production(Arc<RtdNotifierInner>),
    Test(Arc<crate::rtd::test_support::TestNotifierState>),
}

pub(super) struct RtdNotifierInner {
    callback: Arc<RetainedUpdateCallback>,
    operations: Arc<ServerOperationBarrier>,
}

impl RtdNotifierInner {
    fn notify(&self) -> XllResult<()> {
        let _operation = self
            .operations
            .enter_notification()
            .ok_or(XllError::Closing)?;
        self.callback.notify()
    }
}

impl RtdNotifier {
    pub(super) fn new(
        callback: Arc<RetainedUpdateCallback>,
        operations: Arc<ServerOperationBarrier>,
    ) -> Self {
        let inner = Arc::new(RtdNotifierInner {
            callback,
            operations,
        });
        #[cfg(not(test))]
        {
            Self { inner }
        }
        #[cfg(test)]
        {
            Self {
                inner: RtdNotifierKind::Production(inner),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(state: Arc<crate::rtd::test_support::TestNotifierState>) -> Self {
        Self {
            inner: RtdNotifierKind::Test(state),
        }
    }

    pub(crate) fn notify(&self) -> XllResult<()> {
        #[cfg(not(test))]
        {
            self.inner.notify()
        }
        #[cfg(test)]
        {
            match &self.inner {
                RtdNotifierKind::Production(inner) => inner.notify(),
                RtdNotifierKind::Test(state) => state.notify(),
            }
        }
    }
}

#[repr(C)]
pub(super) struct RtdUpdateEvent {
    vtable: *const RtdUpdateEventVtable,
}

#[repr(C)]
struct RtdUpdateEventVtable {
    query_interface: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        *const crate::win32::GUID,
        *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(*mut RtdUpdateEvent) -> u32,
    release: unsafe extern "system" fn(*mut RtdUpdateEvent) -> u32,
    get_type_info_count: unsafe extern "system" fn(*mut RtdUpdateEvent, *mut u32) -> i32,
    get_type_info:
        unsafe extern "system" fn(*mut RtdUpdateEvent, u32, u32, *mut *mut c_void) -> i32,
    get_ids_of_names: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        *const crate::win32::GUID,
        *const *const u16,
        u32,
        u32,
        *mut i32,
    ) -> i32,
    invoke: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        i32,
        *const crate::win32::GUID,
        u32,
        u16,
        *mut c_void,
        *mut crate::win32::VARIANT,
        *mut c_void,
        *mut u32,
    ) -> i32,
    update_notify: unsafe extern "system" fn(*mut RtdUpdateEvent) -> i32,
    get_heartbeat_interval: unsafe extern "system" fn(*mut RtdUpdateEvent, *mut i32) -> i32,
    set_heartbeat_interval: unsafe extern "system" fn(*mut RtdUpdateEvent, i32) -> i32,
    disconnect: unsafe extern "system" fn(*mut RtdUpdateEvent) -> i32,
}

/// Owns one `IRTDUpdateEvent` reference returned by either COM
/// `QueryInterface` or the Global Interface Table.
pub(super) struct OwnedRtdUpdateEvent {
    pointer: NonNull<RtdUpdateEvent>,
}

impl OwnedRtdUpdateEvent {
    /// # Safety
    /// `pointer` must identify a live `IRTDUpdateEvent` interface and own one
    /// COM reference that this value can release exactly once.
    pub(super) unsafe fn from_raw(pointer: NonNull<RtdUpdateEvent>) -> Self {
        Self { pointer }
    }

    pub(super) fn as_ptr(&self) -> *mut RtdUpdateEvent {
        self.pointer.as_ptr()
    }

    fn notify(&self) -> i32 {
        // SAFETY: the wrapper owns a live interface reference for this call.
        unsafe { ((*(*self.pointer.as_ptr()).vtable).update_notify)(self.pointer.as_ptr()) }
    }
}

impl Drop for OwnedRtdUpdateEvent {
    fn drop(&mut self) {
        // SAFETY: `pointer` is a live interface with exactly one owned
        // reference, and Drop runs exactly once.
        unsafe {
            ((*(*self.pointer.as_ptr()).vtable).release)(self.pointer.as_ptr());
        }
    }
}
