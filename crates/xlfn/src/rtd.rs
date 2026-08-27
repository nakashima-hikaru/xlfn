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
#[cfg(feature = "rtd")]
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
#[cfg(feature = "rtd")]
#[derive(Clone, Copy)]
pub struct RtdCallContext<'call> {
    generation: RtdGenerationAccess<'call>,
    host: ExcelHost<'call>,
}

#[cfg(feature = "rtd")]
#[derive(Clone, Copy)]
pub(crate) struct RtdGenerationAccess<'call> {
    subscriptions: &'call crate::excel_rtd::SubscriptionServiceSlot,
    subscription_host: crate::excel_rtd::RtdSubscriptionHost,
}

#[cfg(feature = "rtd")]
impl<'call> RtdGenerationAccess<'call> {
    pub(crate) const fn new(
        subscriptions: &'call crate::excel_rtd::SubscriptionServiceSlot,
        subscription_host: crate::excel_rtd::RtdSubscriptionHost,
    ) -> Self {
        Self {
            subscriptions,
            subscription_host,
        }
    }
}

#[cfg(feature = "rtd")]
impl<'call> RtdCallContext<'call> {
    pub(crate) const fn new(
        generation: RtdGenerationAccess<'call>,
        host: ExcelHost<'call>,
    ) -> Self {
        Self { generation, host }
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
        let subscriptions = self.generation.read()?;
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

#[cfg(feature = "rtd")]
impl RtdGenerationAccess<'_> {
    pub(crate) fn read(self) -> XllResult<crate::excel_rtd::SubscriptionRuntimeRead> {
        self.subscriptions.read(self.subscription_host)
    }
}

pub(crate) fn observe_subscription(
    subscriptions: &Arc<crate::subscription::SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<crate::subscription::RtdValue> {
    crate::excel_rtd::observe_subscription(subscriptions, key, host)
}
