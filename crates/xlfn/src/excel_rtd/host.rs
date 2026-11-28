use crate::excel_rtd::RtdNotifier;
use crate::ingress::{AdmittedExport, ExportIngress};
use crate::subscription::SubscriptionHost;
use crate::{XllError, XllResult};

#[derive(Clone, Copy, Default)]
pub(crate) struct RtdSubscriptionHost {
    ingress: Option<&'static ExportIngress>,
}

impl RtdSubscriptionHost {
    #[cfg(all(test, feature = "rtd"))]
    pub(crate) const fn detached() -> Self {
        Self { ingress: None }
    }

    #[cfg(feature = "rtd")]
    pub(crate) const fn production(ingress: &'static ExportIngress) -> Self {
        Self {
            ingress: Some(ingress),
        }
    }
}

pub(crate) enum RtdAdmissionGuard {
    Production { _ingress: AdmittedExport<'static> },
    Detached,
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

    fn enter_with<T>(&self, operation: impl FnOnce() -> T) -> XllResult<(Self::AdmissionGuard, T)> {
        let Some(ingress) = self.ingress else {
            return Ok((RtdAdmissionGuard::Detached, operation()));
        };
        let (guard, result) = ingress
            .enter_with_result(operation)
            .map_err(|_| XllError::CLOSING)?;
        Ok((RtdAdmissionGuard::Production { _ingress: guard }, result))
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
