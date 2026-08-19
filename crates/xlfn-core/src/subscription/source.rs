use super::*;

/// # Safety
/// Implementors must ensure that cancellation or disconnection can be safely initiated from any thread.
pub unsafe trait RtdSubscription: Send + 'static {
    fn request_cancel(&self);
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

pub trait RtdSource: Send + Sync + 'static {
    type Value: IntoRtdValue + Send + 'static;
    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>>;
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
        RtdSource::subscribe(
            self,
            topic,
            RtdSink {
                sink,
                _value: PhantomData,
            },
        )
    }
}
