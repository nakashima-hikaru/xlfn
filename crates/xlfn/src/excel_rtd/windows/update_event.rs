use super::IID_IRTD_UPDATE_EVENT;
use super::global_interface_table::{GlobalInterfaceTable, get_git};
use super::module_lifetime;
use super::server_gate::ServerOperationBarrier;
use crate::excel_rtd::notification::NotificationPump;
use crate::sync::{Mutex, MutexGuard};
use crate::win32::S_OK;
use crate::{XllError, XllResult};
use std::num::NonZeroU32;
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
pub(super) use xlfn_host::com_transport::update_event::{OwnedRtdUpdateEvent, RtdUpdateEvent};
use xlfn_kernel::published_owner::PublishedOwner;

#[derive(Default)]
pub(super) struct ServerCallbacks {
    pub(super) active: Option<CallbackPtr>,
    pub(super) records: Vec<PublishedOwner<RetainedUpdateCallback>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct CallbackPtr(pub(super) NonNull<RetainedUpdateCallback>);

// SAFETY: RetainedUpdateCallback is Send and Sync, and its address is stable on heap.
unsafe impl Send for CallbackPtr {}
// SAFETY: RetainedUpdateCallback is Send and Sync, and its address is stable on heap.
unsafe impl Sync for CallbackPtr {}

pub(crate) use crate::drain_wait::windows::ComApartmentGuard;

pub(super) struct GitCookieLease {
    cookie: NonZeroU32,
}

impl GitCookieLease {
    pub(super) fn from_registered(cookie: NonZeroU32) -> Self {
        module_lifetime().git_cookie_registered();
        Self { cookie }
    }

    fn raw(&self) -> u32 {
        self.cookie.get()
    }
}

impl Drop for GitCookieLease {
    fn drop(&mut self) {
        match revoke_git_cookie(self.cookie) {
            Ok(()) => module_lifetime().git_cookie_revoked(),
            Err(error) => {
                module_lifetime().git_cookie_revocation_deferred(self.cookie);
                crate::diagnostics::report_no_unwind("RTD GIT callback revocation", &error);
            }
        }
    }
}

pub(super) struct RetainedUpdateCallback {
    resources: CallbackResources,
    // Test hooks run after the worker join and cookie cleanup.
    #[cfg(test)]
    _drop_hook: Option<CallbackDropHook>,
}

struct ActiveCallback {
    // Join the notification worker before revocation can reenter COM.
    pump: NotificationPump,
    _cookie: GitCookieLease,
}

enum CallbackResources {
    Active(ActiveCallback),
    #[cfg(test)]
    Inert,
}

#[cfg(test)]
struct CallbackDropHook(Arc<dyn Fn() + Send + Sync>);

#[cfg(test)]
impl Drop for CallbackDropHook {
    fn drop(&mut self) {
        (self.0)();
    }
}

fn revoke_git_cookie(cookie: NonZeroU32) -> XllResult<()> {
    let _apartment = ComApartmentGuard::enter()
        .map_err(|code| crate::error::construct::windows_api("CoInitializeEx", code))?;
    // SAFETY: this thread has entered a COM apartment. `get_git` returns one
    // owned GIT wrapper. Its COM reference is released exactly once after the
    // synchronous revocation attempt.
    unsafe {
        let git = get_git().map_err(|status| {
            crate::error::construct::windows_api("CoCreateInstance(IGlobalInterfaceTable)", status)
        })?;
        let status = git.revoke(cookie.get());
        if status >= 0 {
            Ok(())
        } else {
            Err(crate::error::construct::windows_api(
                "IGlobalInterfaceTable::RevokeInterfaceFromGlobal",
                status,
            ))
        }
    }
}

pub(super) fn retry_git_revocation_debt() {
    retry_git_revocation_debt_with(revoke_git_cookie);
}

pub(super) fn retry_git_revocation_debt_with(mut revoke: impl FnMut(NonZeroU32) -> XllResult<()>) {
    let claims = module_lifetime().claim_git_revocation_debt_batch();
    for claim in claims {
        match revoke(claim.cookie()) {
            Ok(()) => claim.resolve(),
            Err(error) => {
                crate::diagnostics::report_no_unwind("RTD GIT callback revocation retry", &error);
                // The unresolved claim is requeued by Drop.
            }
        }
    }
}

impl RetainedUpdateCallback {
    pub(super) fn new(
        cookie: GitCookieLease,
        operations: triomphe::Arc<ServerOperationBarrier>,
    ) -> XllResult<Self> {
        let raw_cookie = cookie.raw();
        let pump = NotificationPump::start_with_state(WorkerGit::default, move |worker| {
            let _operation = operations.enter_notification().ok_or(XllError::Closing)?;
            worker.notify(raw_cookie)
        })?;
        Ok(Self {
            resources: CallbackResources::Active(ActiveCallback {
                pump,
                _cookie: cookie,
            }),
            #[cfg(test)]
            _drop_hook: None,
        })
    }

    #[cfg(test)]
    pub(super) fn inert_for_test(hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Box<Self> {
        Box::new(Self {
            resources: CallbackResources::Inert,
            _drop_hook: hook.map(CallbackDropHook),
        })
    }

    #[cfg(all(test, feature = "handles"))]
    pub(super) fn active_for_test(
        pump: NotificationPump,
        cookie: GitCookieLease,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Box<Self> {
        Box::new(Self {
            resources: CallbackResources::Active(ActiveCallback {
                pump,
                _cookie: cookie,
            }),
            _drop_hook: hook.map(CallbackDropHook),
        })
    }

    fn pump(&self) -> Option<&NotificationPump> {
        match &self.resources {
            CallbackResources::Active(active) => Some(&active.pump),
            #[cfg(test)]
            CallbackResources::Inert => None,
        }
    }

    fn notify(&self) -> XllResult<()> {
        self.pump().map_or(Ok(()), NotificationPump::request)
    }

    pub(super) fn pending_notice(&self) -> Option<u64> {
        self.pump().and_then(NotificationPump::pending)
    }

    pub(super) fn acknowledge(&self, notice: Option<u64>) {
        if let Some(pump) = self.pump() {
            pump.acknowledge(notice);
        }
    }
}

/// Apartment-local state, initialized lazily so idle callbacks do not enter
/// COM and ServerStart still reports only notification-worker spawn errors.
#[derive(Default)]
struct WorkerGit {
    // Fields drop in declaration order: release the system GIT reference
    // before balancing the apartment initialization on the same thread.
    git: Option<GlobalInterfaceTable>,
    apartment: Option<ComApartmentGuard>,
}

impl WorkerGit {
    fn table(&mut self) -> XllResult<&GlobalInterfaceTable> {
        match self.git {
            Some(ref git) => Ok(git),
            None => {
                if self.apartment.is_none() {
                    self.apartment = Some(ComApartmentGuard::enter().map_err(|code| {
                        crate::error::construct::windows_api("CoInitializeEx", code)
                    })?);
                }
                // SAFETY: the apartment guard is now live and outlives the
                // cached GIT reference. Failed acquisition leaves the slot
                // empty, so the next scheduled attempt can retry.
                let git = unsafe { get_git() }.map_err(|_| {
                    crate::error::construct::internal(crate::diagnostics::catalog::GIT_NULL)
                })?;
                Ok(self.git.insert(git))
            }
        }
    }

    fn notify(&mut self, cookie: u32) -> XllResult<()> {
        let git = self.table()?;
        // SAFETY: the worker holds its apartment and GIT reference. The IID
        // is readable and names the interface originally registered under
        // the callback's cookie. Keep each proxy local to its admitted call:
        // Release may call back into COM, and must finish before admission
        // is released; do not pin Excel's callback in the cache.
        let notify_status = unsafe {
            let proxy = git
                .get_interface(cookie, &IID_IRTD_UPDATE_EVENT)
                .map_err(|code| {
                    crate::error::construct::windows_api(
                        "IGlobalInterfaceTable::GetInterfaceFromGlobal",
                        code,
                    )
                })?;
            let event = OwnedRtdUpdateEvent::from_raw(proxy.cast());
            event.notify()
        };

        if notify_status != S_OK {
            return Err(crate::error::construct::windows_api(
                "IRTDUpdateEvent::UpdateNotify",
                notify_status,
            ));
        }
        Ok(())
    }
}

/// Retains the callback lock and storage for one installation. Abandonment
/// releases the lock before destroying the callback and its COM resources.
pub(super) struct PreparedCallbackInstallation<'a> {
    callbacks: MutexGuard<'a, ServerCallbacks>,
    callback: PublishedOwner<RetainedUpdateCallback>,
}

impl<'a> PreparedCallbackInstallation<'a> {
    pub(super) fn prepare(
        callbacks: &'a Mutex<ServerCallbacks>,
        callback: Box<RetainedUpdateCallback>,
    ) -> Self {
        let callback = PublishedOwner::from_box(callback);
        let mut callbacks = callbacks.lock();
        callbacks.records.reserve(1);
        Self {
            callbacks,
            callback,
        }
    }

    /// Publishes the already prepared callback without allocating or running
    /// callback code. The same lock covers the record and its active pointer.
    pub(super) fn install(mut self) -> CallbackPtr {
        let ptr = CallbackPtr(NonNull::from(&*self.callback));
        self.callbacks.records.push(self.callback);
        self.callbacks.active = Some(ptr);
        ptr
    }
}

#[cfg(test)]
pub(super) fn install_callback(
    callbacks: &Mutex<ServerCallbacks>,
    callback: Box<RetainedUpdateCallback>,
) -> CallbackPtr {
    PreparedCallbackInstallation::prepare(callbacks, callback).install()
}

pub(super) fn active_callback(callbacks: &Mutex<ServerCallbacks>) -> Option<CallbackPtr> {
    callbacks.lock().active
}

pub(super) fn drain_callbacks(callbacks: &Mutex<ServerCallbacks>) {
    let records = {
        let mut callbacks = callbacks.lock();
        callbacks.active = None;
        std::mem::take(&mut callbacks.records)
    };

    // Dropping a callback revokes a GIT cookie and can therefore run
    // external, reentrant COM code. The callback mutex must not be held here.
    drop(records);
    retry_git_revocation_debt();
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
pub(crate) struct RtdNotifier {
    callback: CallbackPtr,
    operations: NonNull<ServerOperationBarrier>,
}

// SAFETY: RtdNotifier holds audited pointers into the server's stable lifetime.
unsafe impl Send for RtdNotifier {}
// SAFETY: RtdNotifier holds audited pointers into the server's stable lifetime.
unsafe impl Sync for RtdNotifier {}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RtdNotifier {
    inner: RtdNotifierKind,
}

#[cfg(test)]
#[derive(Clone)]
enum RtdNotifierKind {
    Production {
        callback: CallbackPtr,
        operations: NonNull<ServerOperationBarrier>,
    },
    #[cfg(feature = "rtd")]
    Test(Arc<crate::rtd::test_support::TestNotifierState>),
}

impl RtdNotifier {
    /// Constructs a notifier capability linked to the server's update callback and barrier.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `callback` and `operations` remain valid and
    /// dereferenceable for the entire lifetime of this `RtdNotifier`.
    pub(super) unsafe fn new(
        callback: CallbackPtr,
        operations: NonNull<ServerOperationBarrier>,
    ) -> Self {
        #[cfg(not(test))]
        {
            Self {
                callback,
                operations,
            }
        }
        #[cfg(test)]
        {
            Self {
                inner: RtdNotifierKind::Production {
                    callback,
                    operations,
                },
            }
        }
    }

    #[cfg(all(test, feature = "rtd"))]
    pub(crate) fn for_test(state: Arc<crate::rtd::test_support::TestNotifierState>) -> Self {
        Self {
            inner: RtdNotifierKind::Test(state),
        }
    }

    pub(crate) fn notify(&self) -> XllResult<()> {
        #[cfg(not(test))]
        {
            // SAFETY: operations points to the ServerOperationBarrier owned by RtdServer for the server lifetime.
            let _operation = unsafe { self.operations.as_ref() }
                .enter_notification()
                .ok_or(XllError::Closing)?;
            // SAFETY: callback.0 points to RetainedUpdateCallback pinned in ServerCallbacks.records.
            unsafe { self.callback.0.as_ref() }.notify()
        }
        #[cfg(test)]
        {
            match &self.inner {
                RtdNotifierKind::Production {
                    callback,
                    operations,
                } => {
                    // SAFETY: operations points to the ServerOperationBarrier owned by RtdServer for the server lifetime.
                    let _operation = unsafe { operations.as_ref() }
                        .enter_notification()
                        .ok_or(XllError::Closing)?;
                    // SAFETY: callback.0 points to RetainedUpdateCallback pinned in ServerCallbacks.records.
                    unsafe { callback.0.as_ref() }.notify()
                }
                #[cfg(feature = "rtd")]
                RtdNotifierKind::Test(state) => state.notify(),
            }
        }
    }
}
