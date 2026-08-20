use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SOURCE_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

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

/// Runtime-owned identity for an RTD source.
///
/// The handle is the public source identity. Its internal `Arc` keeps the
/// source alive, but allocation addresses are never exposed as part of the
/// subscription API.
pub struct RtdSourceHandle<S: RtdSource> {
    pub(crate) id: u64,
    pub(crate) source: Arc<S>,
}

impl<S: RtdSource> Clone for RtdSourceHandle<S> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            source: Arc::clone(&self.source),
        }
    }
}

impl<S: RtdSource> std::fmt::Debug for RtdSourceHandle<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtdSourceHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<S: RtdSource> RtdSourceHandle<S> {
    /// Registers a source handle with a process-stable opaque identity.
    pub fn new(source: S) -> XllResult<Self> {
        Self::from_arc(Arc::new(source))
    }

    /// Registers a source handle backed by an existing shared source.
    pub fn from_arc(source: Arc<S>) -> XllResult<Self> {
        let id = NEXT_SOURCE_HANDLE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| XllError::Internal {
                diagnostic_id: crate::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        Ok(Self { id, source })
    }
}

pub(crate) trait RtdSourceRef {
    type Source: RtdSource;

    fn source_key(&self) -> SourceKey;
    fn source_arc(&self) -> Arc<Self::Source>;
}

impl<S: RtdSource> RtdSourceRef for RtdSourceHandle<S> {
    type Source = S;

    fn source_key(&self) -> SourceKey {
        SourceKey::Handle(self.id)
    }

    fn source_arc(&self) -> Arc<Self::Source> {
        Arc::clone(&self.source)
    }
}

impl<S: RtdSource> RtdSourceRef for Arc<S> {
    type Source = S;

    fn source_key(&self) -> SourceKey {
        SourceKey::Arc(Arc::as_ptr(self).cast::<()>() as usize)
    }

    fn source_arc(&self) -> Arc<Self::Source> {
        Arc::clone(self)
    }
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
