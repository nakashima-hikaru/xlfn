use crate::XllError;
use crate::error::IntoXllError;

#[cfg(feature = "async")]
pub(crate) use crate::async_udf::AsyncStopped;
#[cfg(feature = "rtd")]
pub(crate) use crate::excel_rtd::SubscriptionsStopped;
#[cfg(not(feature = "rtd"))]
pub(crate) use crate::excel_rtd_protocol::SubscriptionsStopped;
#[cfg(not(feature = "async"))]
pub(crate) use crate::runtime::AsyncStopped;
pub(crate) use crate::runtime::{
    AddinQuiescence as AddinQuiesced, GenerationCallbacksDetached, GenerationReclaimed,
};

#[cfg(feature = "handles")]
pub(crate) use crate::handle::{FormulaHandleServiceSealed as HandlesSealed, HandlesQuiescent};
#[cfg(not(feature = "handles"))]
pub(crate) use crate::runtime_components::{HandlesQuiescent, HandlesSealed};

/// Classification for a best-effort failure observed after unload safety was
/// established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CleanupIssueKind {
    /// Excel registration metadata could not be disposed or reconciled.
    HostMetadata,
    /// Host-owned memory could not be released and was retained.
    HostMemoryLeak,
    /// Diagnostic output could not be delivered completely.
    DiagnosticLoss,
    /// A worker had panicked, but its thread was successfully joined.
    WorkerPanickedAfterJoin,
    /// A destructor or best-effort disposal hook panicked.
    DisposalPanicked,
    /// Registry entries could not be disposed completely.
    RegistryCleanup,
}

#[derive(Clone, Debug)]
pub(crate) struct CleanupIssue {
    pub(crate) component: &'static str,
    pub(crate) kind: CleanupIssueKind,
    pub(crate) error: XllError,
}

#[derive(Debug, Default)]
pub(crate) struct CloseReport {
    issues: Vec<CleanupIssue>,
}

impl CloseReport {
    pub(crate) fn push(
        &mut self,
        component: &'static str,
        kind: CleanupIssueKind,
        error: XllError,
    ) {
        self.issues.push(CleanupIssue {
            component,
            kind,
            error,
        });
    }

    pub(crate) fn issues(&self) -> &[CleanupIssue] {
        &self.issues
    }
}

/// Records non-fatal disposal problems after [`crate::Addin::quiesce`] has
/// established logical quiescence for the generation.
///
/// Physical DLL unloading additionally requires the explicit
/// [`crate::addin::PhysicallyUnloadableAddin`] contract.
pub struct CleanupReporter<'a> {
    report: &'a mut CloseReport,
}

impl<'a> CleanupReporter<'a> {
    pub(crate) fn new(report: &'a mut CloseReport) -> Self {
        Self { report }
    }

    /// Records a non-fatal cleanup failure after quiescence was established.
    ///
    /// `component` identifies the application resource and `kind` classifies
    /// the cleanup issue. The error is converted into the diagnostic stream.
    pub fn warn(
        &mut self,
        component: &'static str,
        kind: CleanupIssueKind,
        error: impl IntoXllError,
    ) {
        self.report.push(component, kind, error.into_xll_error());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnloadHazard {
    HostCallbackStillRegistered,
    #[allow(
        dead_code,
        reason = "Async hazard variant used in async feature builds"
    )]
    AsyncExecutorStillRunning,
    SubscriptionProducerStillRunning,
    HandleStoreNotQuiescent,
    AddinQuiesceFailed,
    AddinCleanupFailed,
    DiagnosticWorkerStillRunning,
    RegistrationStateUnknown,
    CloseInvariantViolation,
    ExportDrainPreparationFailed,
    RtdGitCallbackStillRegistered,
    RtdGitRevocationDebt,
    RtdComObjectsWithoutResidency,
}

impl UnloadHazard {
    pub(crate) fn shutdown_failure(self) -> crate::shutdown_trace::ShutdownFailure {
        match self {
            Self::HostCallbackStillRegistered | Self::RegistrationStateUnknown => {
                crate::shutdown_trace::ShutdownFailure::UnregisterFailed
            }
            Self::AsyncExecutorStillRunning => {
                crate::shutdown_trace::ShutdownFailure::AsyncShutdownFailed
            }
            Self::SubscriptionProducerStillRunning
            | Self::ExportDrainPreparationFailed
            | Self::RtdGitCallbackStillRegistered
            | Self::RtdGitRevocationDebt
            | Self::RtdComObjectsWithoutResidency => {
                crate::shutdown_trace::ShutdownFailure::RtdShutdownFailed
            }
            Self::HandleStoreNotQuiescent => {
                crate::shutdown_trace::ShutdownFailure::HandleShutdownFailed
            }
            Self::AddinQuiesceFailed => crate::shutdown_trace::ShutdownFailure::AddinShutdownFailed,
            Self::AddinCleanupFailed => crate::shutdown_trace::ShutdownFailure::AddinShutdownFailed,
            Self::DiagnosticWorkerStillRunning => {
                crate::shutdown_trace::ShutdownFailure::DiagnosticsShutdownFailed
            }
            Self::CloseInvariantViolation => {
                crate::shutdown_trace::ShutdownFailure::InvariantViolation
            }
        }
    }
}

pub(crate) struct StopOutcome<T> {
    pub(crate) certificate: T,
    pub(crate) issues: Vec<CleanupIssue>,
}
