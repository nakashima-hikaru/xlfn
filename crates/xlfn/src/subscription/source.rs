#![allow(
    unsafe_code,
    reason = "RTD sinks are audited non-owning capabilities over a runtime-owned publish core"
)]

use super::ErasedSink;
use super::topic::RtdTopic;
use super::value::IntoRtdValue;
use crate::generation::RuntimeGeneration;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::marker::PhantomData;

/// A subscription whose cancellation and disconnection protocol is explicit.
///
/// The subscription object remains uniquely owned by its server. Shutdown
/// closes callback admission before invoking `request_cancel`, then consumes
/// the object through `disconnect_and_wait`. No detached shared cancellation
/// object participates in the ownership graph.
/// # Safety
///
/// `disconnect_and_wait` must not return until every callback and worker that
/// can use any [`RtdSink`] clone issued to this subscription has stopped, and
/// no sink clone may be used afterward. This is the temporal lifetime proof
/// that allows sinks to remain non-owning capabilities.
pub unsafe trait RtdSubscription: Send + 'static {
    fn request_cancel(&self);
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

/// Mutable source-registration authority available only during add-in open.
///
/// Registered sources are transferred as one arena into the generation's RTD
/// runtime. Handles carry identity only and never own their source.
pub(crate) struct SourceRegistration {
    generation: RuntimeGeneration,
    sources: Mutex<Vec<Box<dyn ErasedRtdSource>>>,
}

impl std::fmt::Debug for SourceRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceRegistration")
            .field("generation", &self.generation)
            .field("source_count", &self.sources.lock().len())
            .finish()
    }
}

impl SourceRegistration {
    pub(crate) const fn new(generation: RuntimeGeneration) -> Self {
        Self {
            generation,
            sources: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn register<S: RtdSource>(&self, source: S) -> XllResult<RtdSourceHandle<S>> {
        let mut sources = self.sources.lock();
        let sequence = u64::try_from(sources.len())
            .ok()
            .and_then(|current| current.checked_add(1))
            .ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        sources.push(Box::new(source));
        Ok(RtdSourceHandle::from_id(SourceHandleId {
            generation: self.generation,
            sequence,
        }))
    }

    pub(crate) fn finish(self) -> SourceArena {
        SourceArena {
            generation: self.generation,
            sources: self.sources.into_inner().into_boxed_slice(),
        }
    }
}

/// Unique owner of every RTD source registered for one runtime generation.
///
/// Entries are never individually reclaimed. Subscription state refers to
/// them by [`SourceHandleId`], and the complete arena is reclaimed only after
/// subscription shutdown has drained callbacks and disconnected every
/// subscription.
pub(crate) struct SourceArena {
    generation: RuntimeGeneration,
    sources: Box<[Box<dyn ErasedRtdSource>]>,
}

impl SourceArena {
    pub(crate) fn empty(generation: RuntimeGeneration) -> Self {
        Self {
            generation,
            sources: Box::new([]),
        }
    }

    pub(crate) fn resolve(&self, id: SourceHandleId) -> Option<&dyn ErasedRtdSource> {
        if id.generation != self.generation || id.sequence == 0 {
            return None;
        }
        let index = usize::try_from(id.sequence - 1).ok()?;
        self.sources.get(index).map(Box::as_ref)
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn with_source<S: RtdSource>(
        generation: RuntimeGeneration,
        source: S,
    ) -> XllResult<(Self, RtdSourceHandle<S>)> {
        let registration = SourceRegistration::new(generation);
        let handle = registration.register(source)?;
        Ok((registration.finish(), handle))
    }
}

impl std::fmt::Debug for SourceArena {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceArena")
            .field("generation", &self.generation)
            .field("source_count", &self.sources.len())
            .finish()
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

/// Non-owning identity for a runtime-owned RTD source.
///
/// A handle is valid only for the generation that created it. Copying it does
/// not extend source lifetime; source storage belongs exclusively to the
/// generation's [`SourceArena`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceHandleId {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) sequence: u64,
}

pub struct RtdSourceHandle<S: RtdSource> {
    pub(crate) id: SourceHandleId,
    _source: PhantomData<fn() -> S>,
}

impl<S: RtdSource> Copy for RtdSourceHandle<S> {}

impl<S: RtdSource> Clone for RtdSourceHandle<S> {
    fn clone(&self) -> Self {
        *self
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
    const fn from_id(id: SourceHandleId) -> Self {
        Self {
            id,
            _source: PhantomData,
        }
    }
}

/// Typed non-owning publication capability issued by an active RTD server.
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
    #[cfg_attr(feature = "hotpath", hotpath::measure(impl_type = "RtdSink"))]
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
