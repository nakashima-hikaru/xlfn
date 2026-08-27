//! Private Excel RTD transport shared by the public RTD API and formula handles.
//!
//! This module owns the COM/server adapter.  The generic RTD subscription API
//! remains in [`crate::rtd`], while formula handles depend only on the
//! lifetime capability declared by `crate::handle`.

#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        reason = "Generic RTD operations are private transport support when only handles are enabled"
    )
)]

use crate::XllResult;
#[cfg(feature = "handles")]
use crate::handle::FormulaLifetimeBackend;
use crate::host_api::ExcelHost;
#[cfg(feature = "handles")]
use crate::ingress::ExportIngress;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "rtd/host.rs"]
mod host;
#[cfg(feature = "rtd")]
#[path = "rtd/service.rs"]
mod service;

pub(crate) use host::RtdSubscriptionHost;
#[cfg(feature = "rtd")]
pub(crate) use service::SubscriptionRuntimeRead;
#[cfg(feature = "rtd")]
pub(crate) use service::SubscriptionServiceSlot;

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
#[allow(
    unsafe_code,
    reason = "Windows Excel RTD transport is an intentional raw FFI boundary"
)]
#[path = "rtd/windows.rs"]
mod windows;

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use windows::ComModuleLifetime;

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use windows::RtdNotifier;

#[cfg(all(
    not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))),
    not(all(test, feature = "rtd"))
))]
#[derive(Clone)]
pub(crate) enum RtdNotifier {}

#[cfg(all(
    not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))),
    not(all(test, feature = "rtd"))
))]
impl RtdNotifier {
    pub(crate) fn notify(&self) -> XllResult<()> {
        match *self {}
    }
}

#[cfg(all(
    not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))),
    all(test, feature = "rtd")
))]
#[derive(Clone)]
pub(crate) struct RtdNotifier {
    state: Arc<crate::rtd::test_support::TestNotifierState>,
}

#[cfg(all(
    not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))),
    all(test, feature = "rtd")
))]
impl RtdNotifier {
    pub(crate) fn for_test(state: Arc<crate::rtd::test_support::TestNotifierState>) -> Self {
        Self { state }
    }

    pub(crate) fn notify(&self) -> XllResult<()> {
        self.state.notify()
    }
}

pub(crate) fn logical_quiescence_certified() -> bool {
    let module_quiescent = match crate::module_runtime::global().rtd() {
        Some(rtd) => rtd.is_logically_quiescent(),
        None => true,
    };
    module_quiescent && crate::module_runtime::ingress().phase() == crate::ingress::PHASE_CLOSED
}

#[cfg(any(not(feature = "rtd"), test))]
pub(crate) const fn stopped_subscriptions(
    generation: Option<crate::generation::RuntimeGeneration>,
) -> crate::shutdown::SubscriptionsStopped {
    crate::shutdown::SubscriptionsStopped::issue(generation)
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

    pub(crate) fn is_logically_quiescent(&self) -> bool {
        self.logical_quiescence_certified.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct RtdQuiescent(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RtdQuiescenceError {
    pub(crate) outstanding_git_cookies: usize,
    pub(crate) revocation_debt: usize,
}

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
#[cfg(feature = "handles")]
pub(crate) struct RtdOperationGuard {
    ingress_guard: crate::ingress::AdmittedExport<'static>,
    #[cfg(any(test, feature = "refinement"))]
    trace: Option<crate::shutdown_trace::ShutdownTraceHandle>,
}

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
#[cfg(feature = "handles")]
impl Drop for RtdOperationGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        if let Some(trace) = self.trace.as_ref() {
            trace.record(crate::shutdown_trace::ShutdownEvent::EndRtdOperation);
        }
        let _ = &self.ingress_guard;
    }
}

#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
#[cfg(feature = "handles")]
pub(crate) fn begin_operation<H: FormulaLifetimeBackend + ?Sized>(
    handles: &H,
    ingress: &'static ExportIngress,
) -> XllResult<RtdOperationGuard> {
    #[cfg(any(test, feature = "refinement"))]
    let trace = handles.lifetime_trace();
    let ingress_guard = match ingress
        .enter_with(|| {
            #[cfg(any(test, feature = "refinement"))]
            if let Some(trace) = trace.as_ref() {
                trace.record(crate::shutdown_trace::ShutdownEvent::BeginRtdOperation);
            }
        })
        .into_admitted()
    {
        Ok(ingress_guard) => ingress_guard,
        Err(_) => return Err(crate::XllError::Closing),
    };
    Ok(RtdOperationGuard {
        ingress_guard,
        #[cfg(any(test, feature = "refinement"))]
        trace,
    })
}

#[cfg(any(test, feature = "refinement"))]
pub(crate) fn set_trace_sink(trace: crate::shutdown_trace::ShutdownTraceHandle) {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    windows::set_trace_sink(trace);
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    let _ = trace;
}

#[cfg(feature = "handles")]
pub(crate) fn observe_handle<H: FormulaLifetimeBackend + 'static>(
    handles: Arc<H>,
    ingress: &'static ExportIngress,
    lifetime_key: &str,
    token: &str,
    host: ExcelHost<'_>,
) -> XllResult<()> {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        return windows::observe(handles, ingress, lifetime_key, token, host);
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        let _ = (handles, ingress, lifetime_key, token, host);
        Err(crate::XllError::ExcelApi {
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
        return windows::observe_subscription(subscriptions, key, host);
    }
    #[cfg(not(all(target_os = "windows", feature = "rtd")))]
    {
        let _ = (subscriptions, key, host);
        Err(crate::XllError::ExcelApi {
            function: crate::error::ExcelApiFunction::Rtd,
            failure: crate::error::ExcelApiFailure::Status(
                crate::return_value::ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
            ),
        })
    }
}

#[cfg(feature = "handles")]
pub(crate) fn shutdown_handle_topics<H: FormulaLifetimeBackend + 'static>(
    handles: Arc<H>,
) -> XllResult<()> {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        return windows::shutdown(handles);
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
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
        return windows::shutdown_subscriptions(subscriptions);
    }
    #[cfg(not(all(target_os = "windows", feature = "rtd")))]
    {
        subscriptions.close()
    }
}

#[cfg(any(feature = "rtd", feature = "handles"))]
/// Returns the temporary Excel RTD COM class factory.
///
/// # Safety
/// The three pointers must follow the COM `DllGetClassObject` contract.
#[allow(unsafe_code, reason = "COM export is the raw Excel RTD ABI leaf")]
pub(crate) unsafe fn dll_get_class_object(
    class_id: *const core::ffi::c_void,
    interface_id: *const core::ffi::c_void,
    output: *mut *mut core::ffi::c_void,
) -> i32 {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        // SAFETY: exported COM boundary forwards its pointer contract.
        return unsafe { windows::dll_get_class_object(class_id, interface_id, output) };
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        let _ = (class_id, interface_id);
        if !output.is_null() {
            // SAFETY: the caller supplied a writable COM output pointer.
            unsafe { *output = core::ptr::null_mut() };
        }
        0x8004_0111_u32 as i32
    }
}

#[cfg(any(feature = "rtd", feature = "handles"))]
#[must_use]
pub(crate) fn dll_can_unload_now() -> i32 {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        windows::dll_can_unload_now()
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        1
    }
}

pub(crate) fn wait_for_module_quiescence() -> Result<RtdQuiescent, RtdQuiescenceError> {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        windows::wait_for_module_quiescence()?;
    }
    Ok(RtdQuiescent(()))
}
