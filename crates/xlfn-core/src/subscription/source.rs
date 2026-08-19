use super::*;

/// # Safety
/// Implementors must ensure that cancellation or disconnection can be safely initiated from any thread.
pub unsafe trait RtdSubscription: Send + 'static {
    fn request_cancel(&self);
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

// SAFETY: Box<dyn RtdSubscription> forwards directly to the inner RtdSubscription implementation.
unsafe impl RtdSubscription for Box<dyn RtdSubscription> {
    fn request_cancel(&self) {
        (**self).request_cancel();
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        (*self).disconnect_and_wait()
    }
}

pub trait RtdSource: Send + Sync + 'static {
    type Value: IntoRtdValue + Send + 'static;
    type Subscription: RtdSubscription;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription>;
}

pub struct RtdSink<T> {
    pub(crate) sink: ErasedSink,
    pub(crate) _value: PhantomData<fn(T)>,
}

impl<T> Clone for RtdSink<T> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> RtdSink<T>
where
    T: IntoRtdValue,
{
    pub fn publish(&self, value: T) -> XllResult<()> {
        let value = value.into_rtd_value()?;
        value.validate()?;
        self.sink.publish(value)
    }
}

pub(crate) trait ErasedRtdSource: Send + Sync {
    fn subscribe(&self, topic: &RtdTopic, sink: ErasedSink) -> XllResult<Box<dyn RtdSubscription>>;
}

impl<S> ErasedRtdSource for S
where
    S: RtdSource,
{
    fn subscribe(&self, topic: &RtdTopic, sink: ErasedSink) -> XllResult<Box<dyn RtdSubscription>> {
        let sub = RtdSource::subscribe(
            self,
            topic,
            RtdSink {
                sink,
                _value: PhantomData,
            },
        )?;
        Ok(Box::new(sub))
    }
}
