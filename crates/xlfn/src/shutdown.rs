use crate::generation::RuntimeGeneration;
use crate::{IntoXllError, XllError};

/// Classification for a best-effort failure observed after unload safety was
/// established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupIssueKind {
    HostMetadata,
    HostMemoryLeak,
    DiagnosticLoss,
    WorkerPanickedAfterJoin,
    DisposalPanicked,
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

    #[cfg(feature = "async")]
    pub(crate) fn extend(&mut self, issues: impl IntoIterator<Item = CleanupIssue>) {
        self.issues.extend(issues);
    }

    pub(crate) fn issues(&self) -> &[CleanupIssue] {
        &self.issues
    }
}

/// Records non-fatal disposal problems after [`crate::Addin::quiesce`] has
/// established that unloading the XLL is safe.
pub struct CleanupReporter<'a> {
    report: &'a mut CloseReport,
}

impl<'a> CleanupReporter<'a> {
    pub(crate) fn new(report: &'a mut CloseReport) -> Self {
        Self { report }
    }

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
    HandleRuntimeNotQuiescent,
    AddinGenerationEscaped,
    AddinQuiesceFailed,
    DiagnosticWorkerStillRunning,
    RegistrationStateUnknown,
    CloseInvariantViolation,
    RtdGitCallbackStillRegistered,
    RtdGitRevocationDebt,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl UnloadHazard {
    pub(crate) fn ghost_failure(self) -> crate::shutdown_refinement::GhostFailure {
        match self {
            Self::HostCallbackStillRegistered | Self::RegistrationStateUnknown => {
                crate::shutdown_refinement::GhostFailure::UnregisterFailed
            }
            Self::AsyncExecutorStillRunning => {
                crate::shutdown_refinement::GhostFailure::AsyncShutdownFailed
            }
            Self::SubscriptionProducerStillRunning
            | Self::RtdGitCallbackStillRegistered
            | Self::RtdGitRevocationDebt => {
                crate::shutdown_refinement::GhostFailure::RtdShutdownFailed
            }
            Self::HandleRuntimeNotQuiescent => {
                crate::shutdown_refinement::GhostFailure::HandleShutdownFailed
            }
            Self::AddinGenerationEscaped => {
                crate::shutdown_refinement::GhostFailure::GenerationEscaped
            }
            Self::AddinQuiesceFailed => {
                crate::shutdown_refinement::GhostFailure::AddinShutdownFailed
            }
            Self::DiagnosticWorkerStillRunning => {
                crate::shutdown_refinement::GhostFailure::DiagnosticsShutdownFailed
            }
            Self::CloseInvariantViolation => {
                crate::shutdown_refinement::GhostFailure::InvariantViolation
            }
        }
    }
}

pub(crate) struct StopOutcome<T> {
    pub(crate) certificate: T,
    pub(crate) issues: Vec<CleanupIssue>,
}

macro_rules! shutdown_token {
    ($name:ident) => {
        #[derive(Debug)]
        pub(crate) struct $name {
            _private: (),
        }

        impl $name {
            pub(crate) const fn new() -> Self {
                Self { _private: () }
            }
        }
    };
}

shutdown_token!(HostCallbacksDetached);
shutdown_token!(AsyncStopped);
shutdown_token!(SubscriptionsStopped);
shutdown_token!(HandleRegistrySealed);
shutdown_token!(AddinQuiesced);
shutdown_token!(GenerationReclaimed);

/// Proof that the handle registry for one specific runtime generation has no
/// remaining pins. The generation identity travels with the proof so a
/// certificate cannot be silently reused for a different service instance.
#[derive(Debug)]
pub(crate) struct HandlesQuiescent {
    generation: Option<RuntimeGeneration>,
}

impl HandlesQuiescent {
    pub(crate) const fn new(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }

    pub(crate) const fn generation(&self) -> Option<RuntimeGeneration> {
        self.generation
    }
}
