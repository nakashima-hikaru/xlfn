#[cfg(not(target_os = "windows"))]
use crate::XllError;
use crate::XllResult;
use crate::handle::HandleRuntime;
use crate::host_callback::HostCallbackSession;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "windows")]
mod windows;

static MODULE_UNLOAD_CERTIFIED: AtomicBool = AtomicBool::new(false);

pub(crate) fn begin_module_open() {
    MODULE_UNLOAD_CERTIFIED.store(false, Ordering::Release);
}

pub(crate) fn begin_module_close() {
    MODULE_UNLOAD_CERTIFIED.store(false, Ordering::Release);
}

#[cfg(any(test, feature = "shutdown-refinement"))]
pub(crate) fn set_ghost(ghost: crate::shutdown_refinement::GhostHandle) {
    #[cfg(target_os = "windows")]
    windows::set_ghost(ghost);
    #[cfg(not(target_os = "windows"))]
    let _ = ghost;
}

pub(crate) fn certify_module_unload() {
    MODULE_UNLOAD_CERTIFIED.store(true, Ordering::Release);
}

#[cfg_attr(
    not(target_os = "windows"),
    allow(dead_code, reason = "Used by COM integration on Windows")
)]
pub(crate) fn module_unload_certified() -> bool {
    MODULE_UNLOAD_CERTIFIED.load(Ordering::Acquire)
        && crate::ingress::global_ingress().phase() == crate::ingress::PHASE_CLOSED
}

#[derive(Debug)]
pub(crate) struct RtdQuiescent(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RtdQuiescenceError {
    pub(crate) outstanding_git_cookies: usize,
    pub(crate) revocation_debt: usize,
}

pub(crate) fn observe(
    handles: Arc<HandleRuntime>,
    key: &str,
    token: &str,
    callbacks: &HostCallbackSession,
) -> XllResult<()> {
    #[cfg(target_os = "windows")]
    {
        windows::observe(handles, key, token, callbacks)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (handles, key, token, callbacks);
        Err(XllError::ExcelApi {
            function: "xlfRtd",
            code: xlfn_sys::XLRET_FAILED,
        })
    }
}

pub(crate) fn observe_subscription(
    subscriptions: Arc<crate::subscription::SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    callbacks: &HostCallbackSession,
) -> XllResult<crate::RtdValue> {
    #[cfg(target_os = "windows")]
    {
        windows::observe_subscription(subscriptions, key, callbacks)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (subscriptions, key, callbacks);
        Err(XllError::ExcelApi {
            function: "xlfRtd",
            code: xlfn_sys::XLRET_FAILED,
        })
    }
}

pub(crate) fn shutdown(handles: Arc<HandleRuntime>) -> XllResult<()> {
    #[cfg(target_os = "windows")]
    {
        windows::shutdown(handles)
    }
    #[cfg(not(target_os = "windows"))]
    {
        handles.terminate_all_topics();
        Ok(())
    }
}

pub(crate) fn shutdown_subscriptions(
    subscriptions: Arc<crate::subscription::SubscriptionRuntime>,
) -> XllResult<()> {
    #[cfg(target_os = "windows")]
    {
        windows::shutdown_subscriptions(subscriptions)
    }
    #[cfg(not(target_os = "windows"))]
    {
        subscriptions.close()
    }
}

#[cfg(target_os = "windows")]
/// Returns the temporary RTD COM class factory.
///
/// # Safety
/// The three pointers must follow the COM `DllGetClassObject` contract.
pub unsafe fn dll_get_class_object(
    class_id: *const core::ffi::c_void,
    interface_id: *const core::ffi::c_void,
    output: *mut *mut core::ffi::c_void,
) -> i32 {
    // SAFETY: exported COM boundary forwards its pointer contract.
    unsafe { windows::dll_get_class_object(class_id, interface_id, output) }
}

#[cfg(not(target_os = "windows"))]
/// Reports that the RTD COM class is unavailable on non-Windows targets.
///
/// # Safety
/// A non-null `output` must point to writable pointer storage.
pub unsafe fn dll_get_class_object(
    _class_id: *const core::ffi::c_void,
    _interface_id: *const core::ffi::c_void,
    output: *mut *mut core::ffi::c_void,
) -> i32 {
    if !output.is_null() {
        // SAFETY: the caller supplied a writable COM output pointer.
        unsafe { *output = core::ptr::null_mut() };
    }
    0x8004_0111_u32 as i32
}

#[must_use]
pub fn dll_can_unload_now() -> i32 {
    #[cfg(target_os = "windows")]
    {
        windows::dll_can_unload_now()
    }
    #[cfg(not(target_os = "windows"))]
    {
        1 // S_FALSE: the COM server is unavailable on non-Windows targets.
    }
}

pub(crate) fn wait_for_module_quiescence() -> Result<RtdQuiescent, RtdQuiescenceError> {
    #[cfg(target_os = "windows")]
    {
        windows::wait_for_module_quiescence()?;
    }
    Ok(RtdQuiescent(()))
}
