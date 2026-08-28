use crate::XllError;
use crate::error::IntoXllError;
use crate::generation::RuntimeGeneration;

#[cfg(feature = "async")]
pub(crate) use crate::async_udf::AsyncStopped;
#[cfg(not(feature = "async"))]
mod non_async_tokens {
    macro_rules! shutdown_token {
        ($name:ident) => {
            #[derive(Debug)]
            pub(crate) struct $name {
                _private: (),
            }

            impl $name {
                // Issuance stays in the shutdown domain; callers can only
                // consume the resulting proof values.
                pub(crate) const fn issue() -> Self {
                    Self { _private: () }
                }
            }
        };
    }

    shutdown_token!(AsyncStopped);
}
#[cfg(not(feature = "async"))]
pub(crate) use non_async_tokens::AsyncStopped;

mod certificate_tokens {
    macro_rules! shutdown_token {
        ($name:ident) => {
            #[derive(Debug)]
            pub(crate) struct $name {
                _private: (),
            }

            impl $name {
                // Issuance stays in the shutdown domain; callers can only
                // consume the resulting proof values.
                pub(crate) const fn issue() -> Self {
                    Self { _private: () }
                }
            }
        };
    }

    shutdown_token!(AddinQuiesced);
    shutdown_token!(GenerationReclaimed);
    shutdown_token!(HostCallbacksDetached);
}
pub(crate) use certificate_tokens::{AddinQuiesced, GenerationReclaimed, HostCallbacksDetached};

/// Teardown work owned by the handle implementation after its public
/// bindings have been sealed.  The lifecycle core only sees this narrow
/// protocol and never names a handle implementation type.
pub(crate) trait HandleStoreTeardown {
    fn finish(self: Box<Self>) -> crate::XllResult<HandlesQuiescent>;
}

/// Feature-neutral handle shutdown token used by the generation teardown.
///
/// The concrete handle implementation is type-erased behind
/// [`HandleStoreTeardown`].  This keeps the core lifecycle protocol
/// independent from the optional handle subsystem without adding any work to
/// the call or lookup hot paths.
pub(crate) struct HandlesSealed {
    generation: Option<RuntimeGeneration>,
    teardown: Option<Box<dyn HandleStoreTeardown>>,
}

impl HandlesSealed {
    pub(crate) fn empty(generation: Option<RuntimeGeneration>) -> Self {
        Self {
            generation,
            teardown: None,
        }
    }

    #[cfg(feature = "handles")]
    pub(crate) fn from_teardown<T>(generation: Option<RuntimeGeneration>, teardown: T) -> Self
    where
        T: HandleStoreTeardown + 'static,
    {
        Self {
            generation,
            teardown: Some(Box::new(teardown)),
        }
    }

    pub(crate) fn finish(mut self) -> crate::XllResult<HandlesQuiescent> {
        match self.teardown.take() {
            Some(teardown) => teardown.finish(),
            None => Ok(HandlesQuiescent::new(self.generation)),
        }
    }
}

/// Proof that the handle capability for one runtime generation has completed
/// its object and lease quiescence check.
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

/// Feature-neutral proof that the generation subscription producers stopped.
/// The RTD implementation issues this token; the lifecycle core only consumes
/// it as part of the teardown proof.
#[derive(Debug)]
pub(crate) struct SubscriptionsStopped {
    generation: Option<RuntimeGeneration>,
}

impl SubscriptionsStopped {
    pub(crate) const fn issue(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }

    pub(crate) const fn generation(&self) -> Option<RuntimeGeneration> {
        self.generation
    }
}

#[derive(Debug)]
pub(crate) struct ReturnsQuiescent {
    _private: (),
}

impl ReturnsQuiescent {
    fn issue() -> Self {
        Self { _private: () }
    }
}

pub(crate) fn wait_for_return_quiescence(
    protocol: &crate::runtime_components::ReturnProtocol,
) -> crate::XllResult<ReturnsQuiescent> {
    protocol.wait_for_returns();
    if protocol.returns_closed_and_quiescent() {
        Ok(ReturnsQuiescent::issue())
    } else {
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
        })
    }
}

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
    HandleStoreNotQuiescent,
    AddinGenerationEscaped,
    AddinQuiesceFailed,
    AddinCleanupFailed,
    DiagnosticWorkerStillRunning,
    RegistrationStateUnknown,
    CloseInvariantViolation,
    RtdGitCallbackStillRegistered,
    RtdGitRevocationDebt,
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
            | Self::RtdGitCallbackStillRegistered
            | Self::RtdGitRevocationDebt => {
                crate::shutdown_trace::ShutdownFailure::RtdShutdownFailed
            }
            Self::HandleStoreNotQuiescent => {
                crate::shutdown_trace::ShutdownFailure::HandleShutdownFailed
            }
            Self::AddinGenerationEscaped => {
                crate::shutdown_trace::ShutdownFailure::GenerationEscaped
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
