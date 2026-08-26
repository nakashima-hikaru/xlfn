#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The RTD implementation is private in core-only builds"
    )
)]

use crate::XllResult;
use crate::host_api::ExcelHost;
#[cfg(any(feature = "rtd", test))]
use crate::runtime_components::GenerationServices;
#[cfg(any(feature = "rtd", test))]
pub use crate::subscription::{
    IntoRtdValue, RtdCancellation, RtdCancellationHandle, RtdCapacity, RtdLimits, RtdSink,
    RtdSource, RtdSourceHandle, RtdSubscription, RtdTopic, RtdValue,
};
use std::sync::Arc;

#[cfg(test)]
pub(crate) mod test_support;

/// Call-scoped RTD capability borrowed from one coherent generation
/// publication.
///
/// The capability owns the RTD-side preparation/observation/commit protocol;
/// [`crate::addin::MainThreadContext`] only exposes this narrow entry point and
/// does not know how generation services are composed.
#[cfg(any(feature = "rtd", test))]
#[derive(Clone, Copy)]
pub struct RtdCallContext<'call> {
    generation: RtdGenerationAccess<'call>,
    host: ExcelHost<'call>,
}

#[cfg(any(feature = "rtd", test))]
#[derive(Clone, Copy)]
struct RtdGenerationAccess<'call> {
    services: &'call GenerationServices,
}

#[cfg(any(feature = "rtd", test))]
impl<'call> RtdCallContext<'call> {
    pub(crate) const fn new(services: &'call GenerationServices, host: ExcelHost<'call>) -> Self {
        Self {
            generation: RtdGenerationAccess { services },
            host,
        }
    }

    /// Prepares, observes, and commits one RTD subscription transaction.
    ///
    /// A failed Excel observation consumes the prepared transaction through
    /// rollback, so no caller can accidentally publish a pending subscription.
    pub fn subscribe<Source>(
        &self,
        source: &crate::subscription::RtdSourceHandle<Source>,
        topic: crate::subscription::RtdTopic,
    ) -> XllResult<crate::subscription::RtdValue>
    where
        Source: crate::subscription::RtdSource,
    {
        let subscriptions = self
            .generation
            .services
            .subscriptions_slot()
            .read(self.generation.services.subscription_host())?;
        let subscriptions = subscriptions.as_arc();
        let prepared = subscriptions.prepare(source, topic)?;
        match observe_subscription(subscriptions, prepared.key(), self.host) {
            Ok(value) => {
                prepared.commit();
                Ok(value)
            }
            Err(error) => {
                prepared.rollback();
                Err(error)
            }
        }
    }
}

pub(crate) fn observe_subscription(
    subscriptions: &Arc<crate::subscription::SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<crate::subscription::RtdValue> {
    crate::excel_rtd::observe_subscription(subscriptions, key, host)
}
