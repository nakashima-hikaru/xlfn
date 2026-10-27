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
/// Generation teardown is complete; this does not certify physical DLL unload.
pub(crate) struct RtdQuiescent(());

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RtdQuiescenceError {
    pub(crate) outstanding_git_cookies: usize,
    pub(crate) revocation_debt: usize,
    pub(crate) missing_module_residency: bool,
}

/// Physical COM ownership survives a logical close and must seed the next
/// generation's trace before late releases can be recorded into that trace.
#[cfg(any(test, feature = "refinement"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RtdTraceSnapshot {
    pub(crate) rtd_class_factories: u64,
    pub(crate) rtd_servers: u64,
    pub(crate) rtd_server_locks: u64,
}

#[cfg(any(test, feature = "refinement"))]
pub(crate) fn connect_trace<F>(
    trace: crate::shutdown_trace::ShutdownTraceHandle,
    initialize: F,
) -> crate::XllResult<()>
where
    F: FnOnce(RtdTraceSnapshot) -> crate::XllResult<()>,
{
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        crate::excel_rtd::connect_trace(trace, initialize)
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        let _ = trace;
        initialize(RtdTraceSnapshot::default())
    }
}

#[cfg(any(test, feature = "refinement"))]
pub(crate) fn trace_snapshot() -> RtdTraceSnapshot {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        crate::excel_rtd::trace_snapshot()
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        RtdTraceSnapshot::default()
    }
}

/// Physical unloading requires every COM reference, lock, call and GIT
/// registration to be gone even after logical teardown has completed.
pub(crate) fn physical_unload_ready() -> bool {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    {
        crate::excel_rtd::physical_unload_ready()
    }
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    {
        true
    }
}

pub(crate) fn wait_for_module_quiescence(
    module_residency_held: bool,
) -> Result<RtdQuiescent, RtdQuiescenceError> {
    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
    crate::excel_rtd::wait_for_transport_quiescence(module_residency_held)?;
    #[cfg(not(all(target_os = "windows", any(feature = "rtd", feature = "handles"))))]
    let _ = module_residency_held;
    Ok(RtdQuiescent(()))
}
