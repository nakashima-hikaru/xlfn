//! Host port used by the subscription engine.

use crate::XllResult;

pub(crate) trait SubscriptionHost: Clone + Send + Sync + 'static {
    type AdmissionGuard;
    type Notifier: Clone + Send + Sync + 'static;

    fn enter_with<F>(&self, operation: F) -> XllResult<Self::AdmissionGuard>
    where
        F: FnOnce() -> XllResult<()>;

    fn notify(&self, notifier: &Self::Notifier) -> XllResult<()>;
}
