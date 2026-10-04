#![allow(
    unsafe_code,
    reason = "RTD sinks are audited non-owning capabilities over a runtime-owned publish core"
)]

use super::ErasedSink;
use super::topic::RtdTopic;
use super::value::IntoRtdValue;
use crate::generation::RuntimeGeneration;
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::marker::PhantomData;

/// A subscription whose cancellation and disconnection protocol is explicit.
///
/// The subscription object has one owner: its server, or the generation's
/// bounded cleanup queue after COM disconnection revokes the connection.
/// Deferred COM cleanup and shutdown invoke `request_cancel`, then consume
/// the object through `disconnect_and_wait`. Shutdown drains this work before
/// reclaiming its server or source. No detached shared cancellation object
/// participates in the ownership graph.
/// # Safety
///
/// `disconnect_and_wait` must stop every callback and worker that can use any
/// [`RtdSink`] clone issued to this subscription before control leaves the
/// method, whether it returns `Ok`, returns `Err`, or unwinds. No sink clone
/// may be used after any of those exits. Preserve this guarantee during
/// unwinding, for example with a cleanup guard that joins every sink user.
/// This barrier also applies when no prior `request_cancel` call was made.
/// Cancellation and disconnection must not synchronously initiate this
/// add-in's own removal: removal waits for this subscription's cleanup.
/// These methods may run on different framework-owned threads, including a
/// Windows MTA cleanup worker. They must not rely on the subscribing thread's
/// affinity or pass an apartment-bound COM interface without marshaling.
///
/// The framework contains cleanup panics and may reclaim the publish core
/// afterward. Neither an error nor a panic extends the lifetime of a sink.
pub unsafe trait RtdSubscription: Send + 'static {
    /// Requests that subscription work stop without transferring ownership.
    fn request_cancel(&self);
    /// Consumes the subscription and stops all sink users before returning.
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
    #[cfg(any(test, feature = "bench-internals"))]
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

    #[cfg(test)]
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

/// A source that can issue non-owning sinks into its subscription workers.
///
/// Prefer [`super::channel::RtdChannelSource`] for ordinary producers. It
/// contains the sink lifetime proof within the framework.
///
/// # Safety
///
/// An implementation must uphold the sink transfer protocol for every call
/// to [`RtdSource::subscribe`]:
///
/// - if `subscribe` returns `Err` or unwinds, no clone of the supplied sink
///   may escape the call or be used afterward;
/// - if `subscribe` returns `Ok`, every sink clone that may still be used must
///   be owned by the returned [`RtdSubscription`]'s cancellation and
///   disconnection protocol; and
/// - all sink users must stop before `disconnect_and_wait` returns `Ok`,
///   returns `Err`, or unwinds, and no such sink clone may be used afterward.
///
/// This contract is required because [`RtdSink`] is a lifetime-less,
/// non-owning capability into a runtime-owned publish core.
pub unsafe trait RtdSource: Send + Sync + 'static {
    /// Owned values accepted by this source's publication capability.
    type Value: IntoRtdValue + Send + 'static;
    /// The unique owner of every sink user created during subscription.
    type Subscription: RtdSubscription;

    /// Creates a subscription under the sink-transfer safety contract above.
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
/// generation's source arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceHandleId {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) sequence: u64,
}

/// Opaque identity returned by `RtdOpenContext::register_source`.
///
/// Copies refer to the same registered source. Registering another source
/// creates a distinct identity, even if it has the same type or configuration.
/// The runtime owns source storage; this handle does not extend its lifetime
/// and cannot be used in a later add-in open generation.
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
    sink: ErasedSink,
    _value: PhantomData<fn(T)>,
}

impl<T> RtdSink<T> {
    #[cfg(feature = "rtd")]
    pub(super) fn erased(&self) -> ErasedSink {
        self.sink
    }

    #[cfg(feature = "rtd")]
    pub(super) fn publisher_queue(
        &self,
    ) -> XllResult<triomphe::Arc<super::channel::PublisherQueue>> {
        self.sink.publisher_queue()
    }
}

impl<T> Clone for RtdSink<T> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink,
            _value: PhantomData,
        }
    }
}

impl<T> RtdSink<T>
where
    T: IntoRtdValue,
{
    /// Converts, validates, and publishes a value to the active subscription.
    ///
    /// Successful publication updates runtime storage; Excel may coalesce
    /// notifications, so it does not guarantee that a cell displayed every
    /// intermediate value. The subscription's disconnection barrier governs
    /// the lifetime of this non-owning capability and all its clones.
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
