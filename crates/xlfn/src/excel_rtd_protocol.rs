//! Module-lifetime state and shutdown evidence shared with the runtime core.
//!
//! This protocol remains available without either RTD feature. The transport
//! and subscription implementations are compiled only when a feature needs
//! them; a feature-off close still certifies the core lifecycle transition.

use std::sync::atomic::{AtomicBool, Ordering};

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
    #[cfg(any(feature = "rtd", feature = "handles"))]
    pub(crate) const fn new() -> Self {
        Self {
            logical_quiescence_certified: AtomicBool::new(false),
        }
    }

    #[cfg(any(feature = "rtd", feature = "handles"))]
    pub(crate) fn begin_open(&self) {
        self.logical_quiescence_certified
            .store(false, Ordering::Release);
    }

    #[cfg(any(feature = "rtd", feature = "handles"))]
    pub(crate) fn begin_close(&self) {
        self.logical_quiescence_certified
            .store(false, Ordering::Release);
    }

    #[cfg(any(feature = "rtd", feature = "handles"))]
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

#[cfg(any(test, feature = "refinement"))]
pub(crate) fn set_trace_sink(trace: crate::shutdown_trace::ShutdownTraceHandle) {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    crate::excel_rtd::set_trace_sink(trace);
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    let _ = trace;
}

pub(crate) fn wait_for_module_quiescence() -> Result<RtdQuiescent, RtdQuiescenceError> {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    crate::excel_rtd::wait_for_transport_quiescence()?;
    Ok(RtdQuiescent(()))
}
