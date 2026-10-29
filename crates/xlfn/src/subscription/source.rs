#![allow(
    unsafe_code,
    reason = "RTD sinks are audited non-owning capabilities over a runtime-owned publish core"
)]

use super::PublishSink;
use super::topic::RtdTopic;
use super::value::IntoRtdValue;
use crate::generation::RuntimeGeneration;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::subscription_owner::{IntoRtdSubscription, RtdSubscription};

#[cfg(all(test, feature = "rtd"))]
mod lifecycle_tests;
#[cfg(all(test, feature = "rtd"))]
mod ticket_tests;

// RuntimeGeneration belongs to an add-in's lifecycle coordinator. Distinct
// coordinators can have the same generation, so source identities also bind
// the registration authority that issued them.
static NEXT_SOURCE_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

/// Mutable source-registration authority available only during add-in open.
///
/// Registered sources are transferred as one arena into the generation's RTD
/// runtime. Handles carry identity only and never own their source.
pub(crate) struct SourceRegistration<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    generation: RuntimeGeneration,
    registration_id: Option<u64>,
    sources: Mutex<Option<C>>,
    marker: PhantomData<fn() -> E>,
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> std::fmt::Debug
    for SourceRegistration<E, C>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceRegistration")
            .field("generation", &self.generation)
            .field(
                "source_count",
                &self.sources.lock().as_ref().map_or(0, C::len),
            )
            .finish()
    }
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> SourceRegistration<E, C> {
    pub(crate) fn new(generation: RuntimeGeneration) -> Self {
        Self {
            generation,
            registration_id: NEXT_SOURCE_REGISTRATION_ID
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current.checked_add(1)
                })
                .ok(),
            sources: Mutex::new(None),
            marker: PhantomData,
        }
    }
    pub(crate) fn register_sources(&self, catalog: C) -> XllResult<C::Handles> {
        let mut sources = self.sources.lock();
        if sources.is_some() {
            return Err(XllError::custom(
                crate::ExcelError::Value,
                "RTD source catalog is already registered",
            ));
        }
        let handles = catalog.handles(self.generation, self.registration_id()?);
        *sources = Some(catalog);
        Ok(handles)
    }
    fn registration_id(&self) -> XllResult<u64> {
        self.registration_id.ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
        })
    }
    pub(crate) fn finish(self) -> SourceArena<E, C> {
        SourceArena {
            generation: self.generation,
            registration_id: self.registration_id,
            sources: self.sources.into_inner().map(C::store),
            marker: PhantomData,
        }
    }
}
impl<E: RtdSubscription> SourceRegistration<E> {
    pub(crate) fn register<S>(&self, source: S) -> XllResult<RtdSourceHandle<S>>
    where
        S: RtdSource,
        S::Subscription: IntoRtdSubscription<E>,
    {
        let registration_id = self.registration_id()?;
        let mut registration = self.sources.lock();
        let sources = &mut registration
            .get_or_insert_with(crate::DynamicRtdSources::default)
            .sources;
        let sequence = u64::try_from(sources.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
            })?;
        sources.push(Box::new(SourceAdapter {
            source,
            _subscription: PhantomData,
        }));
        Ok(RtdSourceHandle::from_id(SourceHandleId {
            generation: self.generation,
            registration: registration_id,
            sequence,
        }))
    }
}

#[cfg(feature = "rtd")]
impl SourceRegistration<crate::BoxedRtdSubscription> {
    pub(crate) fn register_erased<S: RtdSource>(&self, source: S) -> XllResult<RtdSourceHandle> {
        self.register(BoxedSubscriptionSource(source))
            .map(RtdSourceHandle::erase)
    }
}

#[cfg(feature = "rtd")]
struct BoxedSubscriptionSource<S>(S);

// SAFETY: sink transfer and source shutdown delegate to the original source.
// Boxing the successful subscription moves its complete barrier obligation.
#[cfg(feature = "rtd")]
unsafe impl<S: RtdSource> RtdSource for BoxedSubscriptionSource<S> {
    type Value = S::Value;
    type Subscription = crate::BoxedRtdSubscription;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription> {
        self.0
            .subscribe(topic, sink)
            .map(crate::BoxedRtdSubscription::new)
    }

    fn request_stop(&self) {
        self.0.request_stop();
    }

    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.0.shutdown_and_wait()
    }
}

/// Fixed-address owner of the source catalog for one runtime generation.
/// The allocation is retained until all subscription barriers have completed.
pub(crate) struct SourceArena<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    generation: RuntimeGeneration,
    registration_id: Option<u64>,
    sources: Option<C::Stored>,
    marker: PhantomData<fn() -> E>,
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> SourceArena<E, C> {
    pub(crate) fn is_empty(&self) -> bool {
        self.sources
            .as_ref()
            .is_none_or(|sources| sources.as_ref().len() == 0)
    }
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn empty(generation: RuntimeGeneration) -> Self {
        Self {
            generation,
            registration_id: None,
            sources: None,
            marker: PhantomData,
        }
    }
    pub(crate) fn resolve(&self, id: SourceHandleId) -> Option<ResolvedSource<'_, E, C>> {
        if id.generation != self.generation
            || Some(id.registration) != self.registration_id
            || id.sequence == 0
        {
            return None;
        }
        let index = usize::try_from(id.sequence - 1).ok()?;
        let catalog = self.sources.as_ref()?.as_ref();
        (index < catalog.len()).then_some(ResolvedSource {
            catalog,
            index,
            marker: PhantomData,
        })
    }
    /// Catch each source independently so a panic cannot skip later stop hooks.
    pub(crate) fn request_stop(&self) -> XllResult<()> {
        let mut first_error = None;
        if let Some(storage) = &self.sources {
            let catalog = storage.as_ref();
            for index in 0..catalog.len() {
                if catch_no_unwind(AssertUnwindSafe(|| catalog.request_stop(index))).is_err()
                    && first_error.is_none()
                {
                    first_error = Some(XllError::Panic);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    pub(crate) fn shutdown_and_wait(&self) -> XllResult<()> {
        let mut first_error = None;
        if let Some(storage) = &self.sources {
            let catalog = storage.as_ref();
            for index in 0..catalog.len() {
                let result = catch_no_unwind(AssertUnwindSafe(|| catalog.shutdown_and_wait(index)))
                    .unwrap_or(Err(XllError::Panic));
                if let Err(error) = result
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
impl<E: RtdSubscription> SourceArena<E> {
    #[cfg(test)]
    pub(crate) fn with_source<S>(
        generation: RuntimeGeneration,
        source: S,
    ) -> XllResult<(Self, RtdSourceHandle<S>)>
    where
        S: RtdSource,
        S::Subscription: IntoRtdSubscription<E>,
    {
        let registration = SourceRegistration::new(generation);
        let handle = registration.register(source)?;
        Ok((registration.finish(), handle))
    }
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> std::fmt::Debug for SourceArena<E, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceArena")
            .field("generation", &self.generation)
            .field(
                "source_count",
                &self.sources.as_ref().map_or(0, |c| c.as_ref().len()),
            )
            .finish()
    }
}
pub(crate) struct ResolvedSource<'a, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> {
    catalog: &'a C,
    index: usize,
    marker: PhantomData<fn() -> E>,
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ErasedRtdSource<E>
    for ResolvedSource<'_, E, C>
{
    fn subscribe(&self, topic: &RtdTopic, sink: PublishSink) -> XllResult<E> {
        self.catalog.subscribe(self.index, topic, sink)
    }
    fn request_stop(&self) {
        self.catalog.request_stop(self.index);
    }
    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.catalog.shutdown_and_wait(self.index)
    }
}

/// A source that can issue non-owning sinks into its subscription workers.
///
/// Prefer [`super::multiplex::RtdMultiplexedSource`] for shared connections or
/// [`super::channel::RtdChannelSource`] for independent blocking producers.
/// Both keep the sink lifetime proof within the framework.
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
/// Source-owned workers and callbacks must also stop before
/// [`RtdSource::shutdown_and_wait`] returns or unwinds. Source shutdown does
/// not extend any subscription's sink lifetime.
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

    /// Requests that shared source work stop without waiting for it to exit.
    ///
    /// The runtime calls this for every source after new subscription admission
    /// closes, before waiting for already admitted operations or any source.
    /// It may race an in-flight `subscribe`: synchronize shutdown with setup
    /// so no new worker can escape the eventual source shutdown barrier.
    /// Implementations must return promptly, tolerate repeated calls, and
    /// must not initiate removal of this add-in.
    /// Sources whose workers belong entirely to subscriptions need no hook.
    fn request_stop(&self) {}

    /// Stops and joins source-owned workers after all subscriptions disconnect.
    ///
    /// This must complete the worker/callback barrier even when returning an
    /// error or unwinding, and must work without an earlier `request_stop`.
    /// Repeated calls must remain valid. Use a cleanup guard for panicking
    /// shutdown paths, and implement equivalent cleanup when the source drops
    /// without ever being installed in a runtime.
    ///
    /// Lifecycle hooks may run on framework-owned threads. They must not rely
    /// on the opening or subscribing thread's affinity; apartment-bound COM
    /// interfaces require marshaling. Shutdown must not synchronously initiate
    /// removal of this add-in, which waits for these same workers.
    fn shutdown_and_wait(&self) -> XllResult<()> {
        Ok(())
    }
}

/// Non-owning identity for a runtime-owned RTD source.
///
/// A handle is valid only for the generation that created it. Copying it does
/// not extend source lifetime; source storage belongs exclusively to the
/// generation's source arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceHandleId {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) registration: u64,
    pub(crate) sequence: u64,
}

/// Opaque identity returned by RTD source registration.
///
/// Copies refer to the same registered source. Registering another source
/// creates a distinct identity, even if it has the same type or configuration.
/// The runtime owns source storage; this handle does not extend its lifetime
/// and cannot be used by another registration or a later add-in generation.
/// Registration identities remain distinct even when two add-ins have the
/// same lifecycle generation number and source sequence.
///
/// The default form hides the source implementation. Typed registration also
/// returns `RtdSourceHandle<S>`, which can be converted with [`Self::erase`].
pub struct RtdSourceHandle<S = ()> {
    pub(crate) id: SourceHandleId,
    _source: PhantomData<fn() -> S>,
}

impl<S> Copy for RtdSourceHandle<S> {}

impl<S> Clone for RtdSourceHandle<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> std::fmt::Debug for RtdSourceHandle<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RtdSourceHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<S> RtdSourceHandle<S> {
    /// Hides the source's concrete type while preserving the exact registered
    /// identity and generation. This does not allocate or extend its lifetime.
    #[must_use]
    pub const fn erase(self) -> RtdSourceHandle {
        RtdSourceHandle::from_id(self.id)
    }

    pub(crate) const fn from_id(id: SourceHandleId) -> Self {
        Self {
            id,
            _source: PhantomData,
        }
    }
}

/// Typed non-owning publication capability issued by an active RTD server.
pub struct RtdSink<T> {
    sink: PublishSink,
    _value: PhantomData<fn(T)>,
}

impl<T> RtdSink<T> {
    #[cfg(feature = "rtd")]
    pub(super) fn channel_budget(&self) -> triomphe::Arc<super::budget::ChannelBudget> {
        self.sink.channel_budget()
    }

    #[cfg(feature = "rtd")]
    pub(super) fn erased(&self) -> PublishSink {
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

/// One erased adapter per registered source; subscriptions move out by value.
pub(crate) trait ErasedRtdSource<E: RtdSubscription = ()>: Send + Sync {
    fn subscribe(&self, topic: &RtdTopic, sink: PublishSink) -> XllResult<E>;
    fn request_stop(&self);
    fn shutdown_and_wait(&self) -> XllResult<()>;
}

struct SourceAdapter<S: RtdSource, E: RtdSubscription> {
    source: S,
    // No E is retained in the shared source. E need only be Send, never Sync.
    _subscription: PhantomData<fn() -> E>,
}

impl<S, E> ErasedRtdSource<E> for SourceAdapter<S, E>
where
    S: RtdSource,
    E: RtdSubscription,
    S::Subscription: IntoRtdSubscription<E>,
{
    fn subscribe(&self, topic: &RtdTopic, sink: PublishSink) -> XllResult<E> {
        subscribe_typed(&self.source, topic, sink)
    }

    fn request_stop(&self) {
        self.source.request_stop();
    }

    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.source.shutdown_and_wait()
    }
}

pub(crate) fn subscribe_typed<S, E>(source: &S, topic: &RtdTopic, sink: PublishSink) -> XllResult<E>
where
    S: RtdSource,
    E: RtdSubscription,
    S::Subscription: IntoRtdSubscription<E>,
{
    let subscription = source.subscribe(
        topic,
        RtdSink {
            sink,
            _value: PhantomData,
        },
    )?;
    // The unsafe conversion contract preserves every sink obligation, including
    // completing the cancellation barrier if conversion unwinds.
    Ok(subscription.into_rtd_subscription())
}

/// Affine by-value connection owner, transferred between server and cleanup.
pub(crate) struct SourceSubscription<E: RtdSubscription = ()> {
    subscription: Option<E>,
    // The underlying subscription is Send but need not be Sync. Cancellation
    // requires mutable access to this unique ticket, not shared access to T.
    _not_sync: PhantomData<std::cell::Cell<()>>,
}

impl<E: RtdSubscription> SourceSubscription<E> {
    /// # Safety
    /// The source allocation and all sink roots must remain valid until this
    /// owner is consumed or dropped. Runtime/server admissions and declaration
    /// order provide that lifetime across rollback, deferred jobs and shutdown.
    pub(crate) unsafe fn subscribe(
        source: &(impl ErasedRtdSource<E> + ?Sized),
        topic: &RtdTopic,
        sink: PublishSink,
    ) -> XllResult<Self> {
        let subscription = source.subscribe(topic, sink)?;
        Ok(Self {
            subscription: Some(subscription),
            _not_sync: PhantomData,
        })
    }

    pub(crate) fn request_cancel(&mut self) {
        self.subscription
            .as_ref()
            .expect("live source subscription")
            .request_cancel();
    }

    pub(crate) fn disconnect_and_wait(mut self) -> XllResult<()> {
        self.subscription
            .take()
            .expect("live source subscription")
            .disconnect_and_wait()
    }
}

impl<E: RtdSubscription> Drop for SourceSubscription<E> {
    fn drop(&mut self) {
        if let Some(subscription) = self.subscription.take() {
            let result = catch_no_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait()))
                .unwrap_or(Err(XllError::Panic));
            if let Err(error) = result {
                crate::diagnostics::report_no_unwind("RTD source ticket drop", &error);
            }
        }
    }
}
