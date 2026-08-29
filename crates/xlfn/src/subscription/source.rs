use super::ErasedSink;
use super::topic::RtdTopic;
use super::value::IntoRtdValue;
use crate::generation::RuntimeGeneration;
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(test, feature = "bench-internals"))]
static NEXT_INTERNAL_SOURCE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// A thread-safe cancellation capability detached from subscription ownership.
pub trait RtdCancellation: Send + Sync + 'static {
    fn request_cancel(&self);
}

/// Closure-backed cancellation capability for subscription implementations
/// whose connection object itself is not safe to move across threads.
pub struct RtdCancellationHandle {
    action: Arc<dyn Fn() + Send + Sync + 'static>,
}

impl RtdCancellationHandle {
    pub fn new(action: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            action: Arc::new(action),
        }
    }

    pub fn noop() -> Self {
        Self::new(|| {})
    }
}

impl RtdCancellation for RtdCancellationHandle {
    fn request_cancel(&self) {
        (self.action)();
    }
}

/// A subscription whose cancellation and disconnection protocol is explicit.
///
/// Cancellation is exposed through a separate `Send + Sync` capability so the
/// subscription itself does not need to promise that every method is safe to
/// invoke from arbitrary threads.
pub trait RtdSubscription: Send + 'static {
    fn cancellation(&self) -> Arc<dyn RtdCancellation>;
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

#[derive(Debug)]
pub(crate) struct SourceHandleAllocator {
    generation: RuntimeGeneration,
    next_id: AtomicU64,
}

impl SourceHandleAllocator {
    pub(crate) const fn new(generation: RuntimeGeneration) -> Self {
        Self {
            generation,
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn allocate<S: RtdSource>(&self, source: S) -> XllResult<RtdSourceHandle<S>> {
        RtdSourceHandle::from_arc_and_allocator(Arc::new(source), self)
    }

    pub(crate) fn allocate_shared<S: RtdSource>(
        &self,
        source: Arc<S>,
    ) -> XllResult<RtdSourceHandle<S>> {
        RtdSourceHandle::from_arc_and_allocator(source, self)
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
/// The handle is the only public source identity. Its internal `Arc` keeps the
/// source alive, but shared ownership and identity are deliberately separate.
/// Handles are created by one [`crate::OpenContext`] and carry that open
/// generation in their identity, so a handle from an earlier generation cannot
/// be reused by a later subscription runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceHandleId {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) sequence: u64,
}

pub struct RtdSourceHandle<S: RtdSource> {
    pub(crate) id: SourceHandleId,
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
    fn from_arc_and_allocator(
        source: Arc<S>,
        allocator: &SourceHandleAllocator,
    ) -> XllResult<Self> {
        let sequence = allocator
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        Ok(Self::from_identity(source, allocator.generation, sequence))
    }

    fn from_identity(source: Arc<S>, generation: RuntimeGeneration, sequence: u64) -> Self {
        Self {
            id: SourceHandleId {
                generation,
                sequence,
            },
            source,
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn for_internal(generation: RuntimeGeneration, source: S) -> XllResult<Self> {
        let sequence = NEXT_INTERNAL_SOURCE_SEQUENCE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        Ok(Self::from_identity(Arc::new(source), generation, sequence))
    }

    #[cfg(all(test, target_os = "windows"))]
    pub(crate) fn for_internal_shared(
        generation: RuntimeGeneration,
        source: Arc<S>,
    ) -> XllResult<Self> {
        let sequence = NEXT_INTERNAL_SOURCE_SEQUENCE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        Ok(Self::from_identity(source, generation, sequence))
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
    #[hotpath::measure]
    pub fn publish(&self, value: T) -> XllResult<()> {
        let value = value.into_rtd_value()?.into_stored()?;
        self.sink.publish_stored(value)
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
