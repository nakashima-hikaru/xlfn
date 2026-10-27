use crate::excel_rtd::RtdNotifier;
use crate::ingress::{AdmittedExport, ExportIngress};
use crate::subscription::SubscriptionHost;
use crate::{XllError, XllResult};

#[derive(Clone, Copy, Default)]
pub(crate) struct RtdSubscriptionHost {
    ingress: Option<&'static ExportIngress>,
}

impl RtdSubscriptionHost {
    #[cfg(test)]
    pub(crate) const fn detached() -> Self {
        Self { ingress: None }
    }

    pub(crate) const fn production(ingress: &'static ExportIngress) -> Self {
        Self {
            ingress: Some(ingress),
        }
    }
}

pub(crate) struct RtdAdmissionGuard {
    _ingress: Option<AdmittedExport<'static>>,
}

/// External transport ownership released before deferred runtime admission.
pub(crate) enum RtdDisconnectCompletion {
    #[cfg(target_os = "windows")]
    Server {
        _lease: super::windows::DeferredDisconnectLease,
    },
    #[cfg(test)]
    Test { _completion: Box<dyn Send> },
}

#[cfg(target_os = "windows")]
impl From<super::windows::DeferredDisconnectLease> for RtdDisconnectCompletion {
    fn from(lease: super::windows::DeferredDisconnectLease) -> Self {
        Self::Server { _lease: lease }
    }
}

#[cfg(test)]
impl<T: Send + 'static> From<Box<T>> for RtdDisconnectCompletion {
    fn from(completion: Box<T>) -> Self {
        Self::Test {
            _completion: completion,
        }
    }
}

impl SubscriptionHost for RtdSubscriptionHost {
    type AdmissionGuard = RtdAdmissionGuard;
    type Notifier = RtdNotifier;
    type DisconnectCompletion = RtdDisconnectCompletion;

    fn enter_with<F>(&self, operation: F) -> XllResult<Self::AdmissionGuard>
    where
        F: FnOnce() -> XllResult<()>,
    {
        let Some(ingress) = self.ingress else {
            operation()?;
            return Ok(RtdAdmissionGuard { _ingress: None });
        };

        let mut operation_result = None;
        let entry = ingress.enter_with(|| {
            operation_result = Some(operation());
        });
        let guard = match entry.into_admitted() {
            Ok(guard) => guard,
            Err(_) => return Err(XllError::Closing),
        };

        match operation_result.expect("accepted host admission runs its operation") {
            Ok(()) => Ok(RtdAdmissionGuard {
                _ingress: Some(guard),
            }),
            Err(error) => {
                drop(guard);
                Err(error)
            }
        }
    }

    fn notify(&self, notifier: &Self::Notifier) -> XllResult<()> {
        notifier.notify()
    }

    fn publish_sink(
        core: &crate::subscription::PublishCore<Self>,
        topic_id: crate::subscription::SinkTopicId,
        generation: crate::generation::ConnectionGeneration,
    ) -> crate::subscription::PublishSink {
        crate::subscription::PublishSink::for_publish(core, topic_id, generation)
    }
}
