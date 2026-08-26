use crate::excel_rtd::RtdNotifier;
use crate::ingress::{AdmittedExport, ExportIngress};
use crate::subscription::SubscriptionHost;
use crate::{XllError, XllResult};

#[derive(Clone, Copy, Default)]
pub(crate) struct RtdSubscriptionHost {
    ingress: Option<&'static ExportIngress>,
}

impl RtdSubscriptionHost {
    #[cfg(any(test, feature = "bench-internals"))]
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

impl SubscriptionHost for RtdSubscriptionHost {
    type AdmissionGuard = RtdAdmissionGuard;
    type Notifier = RtdNotifier;

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
}
