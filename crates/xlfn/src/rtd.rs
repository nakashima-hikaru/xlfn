#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The RTD implementation is private in core-only builds"
    )
)]

use crate::handle::FormulaHandleService;
use crate::host_api::ExcelHost;
use crate::ingress::ExportIngress;
#[cfg(any(feature = "rtd", test))]
pub use crate::subscription::{
    IntoRtdValue, RtdCancellation, RtdCancellationHandle, RtdLimits, RtdSink, RtdSource,
    RtdSourceHandle, RtdSubscription, RtdTopic, RtdValue,
};
use crate::{XllError, XllResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
pub(crate) mod test_support;

mod host;
pub(crate) mod service;

#[cfg(all(target_os = "windows", feature = "rtd"))]
mod windows;

#[cfg(all(target_os = "windows", feature = "rtd"))]
pub(crate) use windows::{ComModuleLifetime, RtdNotifier};

pub(crate) use host::RtdSubscriptionHost;
#[cfg(any(feature = "rtd", test))]
pub(crate) use service::SubscriptionServiceSlot;
pub(crate) use service::SubscriptionsStopped;

#[cfg(all(not(test), any(not(feature = "rtd"), not(target_os = "windows"))))]
#[derive(Clone)]
pub(crate) enum RtdNotifier {}

#[cfg(all(not(test), any(not(feature = "rtd"), not(target_os = "windows"))))]
impl RtdNotifier {
    pub(crate) fn notify(&self) -> XllResult<()> {
        match *self {}
    }
}

#[cfg(all(test, any(not(feature = "rtd"), not(target_os = "windows"))))]
#[derive(Clone)]
pub(crate) struct RtdNotifier {
    state: Arc<test_support::TestNotifierState>,
}

#[cfg(all(test, any(not(feature = "rtd"), not(target_os = "windows"))))]
impl RtdNotifier {
    pub(crate) fn for_test(state: Arc<test_support::TestNotifierState>) -> Self {
        Self { state }
    }

    pub(crate) fn notify(&self) -> XllResult<()> {
        self.state.notify()
    }
}

pub(crate) struct RtdModuleState {
    logical_quiescence_certified: AtomicBool,
}

impl RtdModuleState {
    pub(crate) const fn new() -> Self {
        Self {
            logical_quiescence_certified: AtomicBool::new(false),
        }
    }

    pub(crate) fn begin_open(&self) {
        self.logical_quiescence_certified
            .store(false, Ordering::Release);
    }

    pub(crate) fn begin_close(&self) {
        self.logical_quiescence_certified
            .store(false, Ordering::Release);
    }

    pub(crate) fn certify_logical_quiescence(&self) {
        self.logical_quiescence_certified
            .store(true, Ordering::Release);
    }

    fn is_logically_quiescent(&self) -> bool {
        self.logical_quiescence_certified.load(Ordering::Acquire)
    }
}

/// Admission guard for one RTD Excel operation.
///
/// This belongs to the RTD adapter rather than the formula-handle service:
/// module ingress is an RTD/COM concern, while the handle service owns only
/// handle and topic state.
#[cfg(all(target_os = "windows", feature = "rtd"))]
pub(crate) struct RtdOperationGuard {
    ingress_guard: crate::ingress::AdmittedExport<'static>,
    #[cfg(any(test, feature = "refinement"))]
    ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

#[cfg(all(target_os = "windows", feature = "rtd"))]
impl Drop for RtdOperationGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
        let _ = &self.ingress_guard;
    }
}

#[cfg(all(target_os = "windows", feature = "rtd"))]
pub(crate) fn begin_operation(
    _handles: &FormulaHandleService,
    ingress: &'static ExportIngress,
) -> XllResult<RtdOperationGuard> {
    #[cfg(any(test, feature = "refinement"))]
    let ghost = _handles.rtd_ghost();
    let ingress_guard = match ingress
        .enter_with(|| {
            #[cfg(any(test, feature = "refinement"))]
            if let Some(ghost) = ghost.as_ref() {
                ghost.record_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
            }
        })
        .into_admitted()
    {
        Ok(ingress_guard) => ingress_guard,
        Err(_) => return Err(XllError::Closing),
    };
    Ok(RtdOperationGuard {
        ingress_guard,
        #[cfg(any(test, feature = "refinement"))]
        ghost,
    })
}

#[cfg(any(test, feature = "refinement"))]
pub(crate) fn set_ghost(ghost: crate::shutdown_refinement::GhostHandle) {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    windows::set_ghost(ghost);
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    let _ = ghost;
}

pub(crate) fn logical_quiescence_certified() -> bool {
    let module_quiescent = match crate::module_runtime::global().rtd() {
        Some(rtd) => rtd.is_logically_quiescent(),
        None => true,
    };
    module_quiescent && crate::module_runtime::ingress().phase() == crate::ingress::PHASE_CLOSED
}

#[derive(Debug)]
pub(crate) struct RtdQuiescent(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RtdQuiescenceError {
    pub(crate) outstanding_git_cookies: usize,
    pub(crate) revocation_debt: usize,
}

pub(crate) fn observe(
    handles: &Arc<FormulaHandleService>,
    ingress: &'static ExportIngress,
    key: &str,
    token: &str,
    host: ExcelHost<'_>,
) -> XllResult<()> {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::observe(handles, ingress, key, token, host)
    }
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    {
        let _ = (handles, ingress, key, token, host);
        Err(XllError::ExcelApi {
            function: crate::error::ExcelApiFunction::Rtd,
            failure: crate::error::ExcelApiFailure::Status(
                crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
            ),
        })
    }
}

pub(crate) fn observe_subscription(
    subscriptions: &Arc<crate::subscription::SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<crate::subscription::RtdValue> {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::observe_subscription(subscriptions, key, host)
    }
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    {
        let _ = (subscriptions, key, host);
        Err(XllError::ExcelApi {
            function: crate::error::ExcelApiFunction::Rtd,
            failure: crate::error::ExcelApiFailure::Status(
                crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
            ),
        })
    }
}

/// Stops the RTD topic server owned by the formula-handle generation.
///
/// This is deliberately named after the handle-side bridge: subscription
/// shutdown is a separate operation below, and the RTD adapter must not make
/// the handle service's ownership boundary look like a generic RTD shutdown.
pub(crate) fn shutdown_handle_topics(handles: Arc<FormulaHandleService>) -> XllResult<()> {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::shutdown(handles)
    }
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    {
        handles.terminate_all_topics();
        Ok(())
    }
}

pub(crate) fn shutdown_subscriptions(
    subscriptions: Arc<crate::subscription::SubscriptionRuntime>,
) -> XllResult<()> {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::shutdown_subscriptions(subscriptions)
    }
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    {
        subscriptions.close()
    }
}

#[cfg(all(target_os = "windows", feature = "rtd"))]
/// Returns the temporary RTD COM class factory.
///
/// # Safety
/// The three pointers must follow the COM `DllGetClassObject` contract.
#[allow(unsafe_code, reason = "COM export is the raw RTD ABI leaf")]
pub(crate) unsafe fn dll_get_class_object(
    class_id: *const core::ffi::c_void,
    interface_id: *const core::ffi::c_void,
    output: *mut *mut core::ffi::c_void,
) -> i32 {
    // SAFETY: exported COM boundary forwards its pointer contract.
    unsafe { windows::dll_get_class_object(class_id, interface_id, output) }
}

#[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
/// Reports that the RTD COM class is unavailable on non-Windows targets.
///
/// # Safety
/// A non-null `output` must point to writable pointer storage.
#[allow(
    unsafe_code,
    reason = "Unavailable COM export still implements the raw ABI contract"
)]
pub(crate) unsafe fn dll_get_class_object(
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
pub(crate) fn dll_can_unload_now() -> i32 {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::dll_can_unload_now()
    }
    #[cfg(any(not(target_os = "windows"), not(feature = "rtd")))]
    {
        1 // S_FALSE: the COM server is unavailable on non-Windows targets.
    }
}

pub(crate) fn wait_for_module_quiescence() -> Result<RtdQuiescent, RtdQuiescenceError> {
    #[cfg(all(target_os = "windows", feature = "rtd"))]
    {
        windows::wait_for_module_quiescence()?;
    }
    Ok(RtdQuiescent(()))
}
