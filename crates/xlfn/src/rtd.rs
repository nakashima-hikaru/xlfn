#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The RTD implementation is private in core-only builds"
    )
)]

#[cfg(feature = "rtd")]
pub use crate::BoxedRtdSubscription;
use crate::XllResult;
use crate::host_api::ExcelHost;
#[cfg(feature = "rtd")]
pub use crate::subscription::{
    IntoRtdSubscription, IntoRtdValue, NoTopicValidator, RtdBatchSendError, RtdCapacity,
    RtdChannelSource, RtdChannelSubscription, RtdDelivery, RtdDeliveryPolicy, RtdFifo, RtdLatest,
    RtdLimits, RtdMultiplexedSource, RtdMultiplexedSubscription, RtdPendingBatch, RtdPendingValue,
    RtdProducerErrorPolicy, RtdSendError, RtdSender, RtdSink, RtdSource, RtdSourceContext,
    RtdSourceEvent, RtdSourceHandle, RtdSourceLimits, RtdSubscription, RtdSubscriptionId, RtdTopic,
    RtdTopicInput, RtdTopicParts, RtdTopicValidator, RtdValue,
};

#[cfg(test)]
pub(crate) mod test_support;

/// Call-scoped RTD capability borrowed from one coherent generation
/// publication.
///
/// The capability owns the RTD-side preparation/observation/commit protocol;
/// [`crate::addin::MainThreadContext`] only exposes this narrow entry point and
/// does not know how generation services are composed.
#[cfg(feature = "rtd")]
pub struct RtdCallContext<
    'call,
    E: crate::RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    generation: RtdGenerationAccess<'call, E, C>,
    host: ExcelHost<'call>,
}

#[cfg(feature = "rtd")]
pub(crate) struct RtdGenerationAccess<
    'call,
    E: crate::RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    subscriptions: &'call crate::excel_rtd::SubscriptionServiceSlot<E, C>,
    subscription_host: crate::excel_rtd::RtdSubscriptionHost,
}

#[cfg(feature = "rtd")]
impl<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RtdGenerationAccess<'call, E, C>
{
    pub(crate) const fn new(
        subscriptions: &'call crate::excel_rtd::SubscriptionServiceSlot<E, C>,
        subscription_host: crate::excel_rtd::RtdSubscriptionHost,
    ) -> Self {
        Self {
            subscriptions,
            subscription_host,
        }
    }
}

#[cfg(feature = "rtd")]
impl<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> RtdCallContext<'call, E, C> {
    pub(crate) const fn new(
        generation: RtdGenerationAccess<'call, E, C>,
        host: ExcelHost<'call>,
    ) -> Self {
        Self { generation, host }
    }

    /// Prepares, observes, and commits one RTD subscription transaction.
    ///
    /// A failed Excel observation consumes the prepared transaction through
    /// rollback, so no caller can accidentally publish a pending subscription.
    /// Accepts a borrowed [`RtdTopic`], array, slice, or `Vec` containing
    /// `String`, `&str`, or references to those string types.
    /// Reusing an existing subscription
    /// does not allocate topic storage; a new subscription owns its canonical copy.
    pub fn subscribe<Source, Parts: RtdTopicInput + ?Sized>(
        &self,
        source: &crate::subscription::RtdSourceHandle<Source>,
        parts: &Parts,
    ) -> XllResult<crate::subscription::RtdValue> {
        let subscriptions = self.generation.read()?;
        let topic = parts.prepare_parts()?;
        let prepared = subscriptions.prepare(source, topic)?;
        match observe_subscription(&subscriptions, prepared.key(), self.host) {
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
impl<'call, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RtdGenerationAccess<'call, E, C>
{
    pub(crate) fn read(self) -> XllResult<crate::excel_rtd::SubscriptionRuntimeRead<'call, E, C>> {
        self.subscriptions.read(self.subscription_host)
    }
}

pub(crate) fn observe_subscription<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    subscriptions: &crate::subscription::SubscriptionRuntime<E, C>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<crate::subscription::RtdValue> {
    crate::excel_rtd::observe_subscription(subscriptions, key, host)
}

#[cfg(feature = "rtd")]
impl<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy for RtdCallContext<'_, E, C> {}
#[cfg(feature = "rtd")]
impl<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone for RtdCallContext<'_, E, C> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "rtd")]
impl<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy
    for RtdGenerationAccess<'_, E, C>
{
}
#[cfg(feature = "rtd")]
impl<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone
    for RtdGenerationAccess<'_, E, C>
{
    fn clone(&self) -> Self {
        *self
    }
}
