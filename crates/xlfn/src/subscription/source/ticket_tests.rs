use super::*;
use crate::excel_rtd::RtdSubscriptionHost;
use crate::generation::ConnectionGeneration;
use crate::subscription::host::SubscriptionHost;
use crate::subscription::{RtdLimits, TopicId};
use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Counts {
    subscribed: AtomicUsize,
    canceled: AtomicUsize,
    disconnected: AtomicUsize,
    dropped: AtomicUsize,
}

type Reentry = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

struct ProbeSource {
    counts: Arc<Counts>,
    reentry: Reentry,
    panic_on_cancel: bool,
}

struct ProbeSubscription {
    counts: Arc<Counts>,
    reentry: Reentry,
    panic_on_cancel: bool,
    local_cancellations: Cell<usize>,
}

static_assertions::assert_impl_all!(ProbeSubscription: Send);
static_assertions::assert_not_impl_any!(ProbeSubscription: Sync);
static_assertions::assert_impl_all!(SourceSubscription<ProbeSubscription>: Send);
static_assertions::assert_not_impl_any!(SourceSubscription<ProbeSubscription>: Sync, Clone, Copy);

type ProbeRuntime =
    crate::subscription::runtime::SubscriptionRuntime<RtdSubscriptionHost, ProbeSubscription>;

impl Drop for ProbeSubscription {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

// SAFETY: the probe never retains a sink or starts asynchronous work. Its
// callbacks only touch owned counters and an explicitly synchronized test hook.
unsafe impl RtdSubscription for ProbeSubscription {
    fn request_cancel(&self) {
        self.local_cancellations
            .set(self.local_cancellations.get() + 1);
        self.counts.canceled.fetch_add(1, Ordering::AcqRel);
        let reenter = self.reentry.lock().take();
        if let Some(reenter) = reenter {
            reenter();
        }
        assert!(!self.panic_on_cancel, "injected ticket cancellation panic");
    }

    fn disconnect_and_wait(self) -> XllResult<()> {
        self.counts.disconnected.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

// SAFETY: subscribe discards its sink immediately. The returned value owns
// only counters and a test callback; there are no escaped sink users to join.
unsafe impl RtdSource for ProbeSource {
    type Value = i32;
    type Subscription = ProbeSubscription;

    fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<Self::Subscription> {
        self.counts.subscribed.fetch_add(1, Ordering::AcqRel);
        Ok(ProbeSubscription {
            counts: Arc::clone(&self.counts),
            reentry: Arc::clone(&self.reentry),
            panic_on_cancel: self.panic_on_cancel,
            local_cancellations: Cell::new(0),
        })
    }
}

fn source(
    generation: RuntimeGeneration,
    counts: &Arc<Counts>,
    reentry: &Reentry,
    panic_on_cancel: bool,
) -> (SourceArena<ProbeSubscription>, RtdSourceHandle<ProbeSource>) {
    SourceArena::with_source(
        generation,
        ProbeSource {
            counts: Arc::clone(counts),
            reentry: Arc::clone(reentry),
            panic_on_cancel,
        },
    )
    .unwrap()
}

fn sink<E: RtdSubscription>(
    server: &crate::subscription::server::SubscriptionServerHandle<RtdSubscriptionHost, E>,
) -> PublishSink {
    RtdSubscriptionHost::publish_sink(
        server.test_server().publish.as_ref(),
        TopicId(1),
        ConnectionGeneration::new(1).unwrap(),
    )
}

#[test]
fn miri_source_ticket_survives_arena_and_runtime_moves() {
    let counts = Arc::new(Counts::default());
    let reentry = Arc::new(Mutex::new(None));
    let (arena, handle) = source(RuntimeGeneration::new(1).unwrap(), &counts, &reentry, false);
    // The independent publish root stays fixed while the source arena and its
    // later owning runtime move. The probe does not retain or use this sink.
    let publish_runtime = Box::new(ProbeRuntime::new());
    let server = publish_runtime.register_test_server(1);
    let adapter = arena.resolve(handle.id).unwrap();
    let adapter_address = NonNull::from(adapter.catalog.sources[adapter.index].as_ref());
    let topic = RtdTopic::single("move-source-owner").unwrap();
    let mut tickets = Vec::new();
    for _ in 0..65 {
        // SAFETY: the arena's boxed source allocation and independent publish
        // root outlive every ticket, including the scoped worker below.
        tickets.push(
            unsafe { SourceSubscription::subscribe(&adapter, &topic, sink(&server)) }.unwrap(),
        );
    }

    let moved_arena = Box::new(arena);
    let mut runtimes = Vec::with_capacity(1);
    runtimes.push(ProbeRuntime::with_sources_for_internal(*moved_arena));
    for _ in 0..3 {
        // Force relocation of the owning runtime's Vec allocation. No raw
        // server handle has ever been created for these movable runtimes.
        runtimes.push(ProbeRuntime::new());
    }
    assert_eq!(
        NonNull::from(runtimes[0].sources.sources.as_ref().unwrap().sources[0].as_ref()),
        adapter_address
    );
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                for mut ticket in tickets {
                    ticket.request_cancel();
                    ticket.disconnect_and_wait().unwrap();
                }
            })
            .join()
            .unwrap();
    });
    assert_eq!(counts.subscribed.load(Ordering::Acquire), 65);
    assert_eq!(counts.canceled.load(Ordering::Acquire), 65);
    assert_eq!(counts.disconnected.load(Ordering::Acquire), 65);
    assert_eq!(counts.dropped.load(Ordering::Acquire), 65);
    for runtime in &runtimes {
        runtime.close().unwrap();
    }
}

#[test]
fn miri_source_ticket_cancel_panic_preserves_owner_and_next_owner_is_independent() {
    let counts = Arc::new(Counts::default());
    let reentry = Arc::new(Mutex::new(None));
    let (arena, handle) = source(RuntimeGeneration::new(1).unwrap(), &counts, &reentry, true);
    let runtime = Box::new(ProbeRuntime::with_sources_for_internal(arena));
    let server = runtime.register_test_server(1);
    let adapter = runtime.sources.resolve(handle.id).unwrap();
    let topic = RtdTopic::single("cancel-restore").unwrap();
    // SAFETY: this fixed runtime and publish root remain alive until both
    // tickets have been consumed, including cancellation's unwind path.
    let mut ticket =
        unsafe { SourceSubscription::subscribe(&adapter, &topic, sink(&server)) }.unwrap();
    assert!(catch_no_unwind(AssertUnwindSafe(|| ticket.request_cancel())).is_err());
    assert_eq!(
        ticket
            .subscription
            .as_ref()
            .unwrap()
            .local_cancellations
            .get(),
        1
    );
    ticket.disconnect_and_wait().unwrap();
    // SAFETY: same fixed source/publish allocations as the previous ticket.
    let reused = unsafe { SourceSubscription::subscribe(&adapter, &topic, sink(&server)) }.unwrap();
    assert_eq!(
        reused
            .subscription
            .as_ref()
            .unwrap()
            .local_cancellations
            .get(),
        0
    );
    drop(reused);
    assert_eq!(counts.canceled.load(Ordering::Acquire), 1);
    assert_eq!(counts.disconnected.load(Ordering::Acquire), 2);
    assert_eq!(counts.dropped.load(Ordering::Acquire), 2);
    runtime.close().unwrap();
}

#[test]
fn miri_source_ticket_cancel_reenters_same_source_without_a_storage_lock() {
    let counts = Arc::new(Counts::default());
    let reentry = Arc::new(Mutex::new(None));
    let (arena, handle) = source(RuntimeGeneration::new(1).unwrap(), &counts, &reentry, false);
    let runtime = Arc::new(ProbeRuntime::with_sources_for_internal(arena));
    let server = runtime.register_test_server(1);
    let topic = RtdTopic::single("outer-cancel").unwrap();
    // SAFETY: the Arc below retains the runtime through this unique ticket's
    // consumption. No callback may outlive the synchronous test hook.
    let mut ticket = unsafe {
        SourceSubscription::subscribe(
            &runtime.sources.resolve(handle.id).unwrap(),
            &topic,
            sink(&server),
        )
    }
    .unwrap();
    let reentrant_runtime = Arc::clone(&runtime);
    *reentry.lock() = Some(Box::new(move || {
        let prepared = reentrant_runtime
            .prepare(
                &handle,
                RtdTopic::single("reentrant-connect").unwrap().borrowed(),
            )
            .unwrap();
        server
            .connect_transaction(TopicId(2), prepared.id())
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
        server.disconnect(TopicId(2)).unwrap();
    }));
    ticket.request_cancel();
    ticket.disconnect_and_wait().unwrap();
    assert_eq!(counts.subscribed.load(Ordering::Acquire), 2);
    assert_eq!(counts.canceled.load(Ordering::Acquire), 1);
    assert_eq!(counts.disconnected.load(Ordering::Acquire), 2);
    assert_eq!(counts.dropped.load(Ordering::Acquire), 2);
    runtime.close().unwrap();
}

#[test]
fn stale_source_handle_cannot_reach_a_new_generation_adapter() {
    let counts = Arc::new(Counts::default());
    let reentry = Arc::new(Mutex::new(None));
    let (old_arena, old_handle) =
        source(RuntimeGeneration::new(1).unwrap(), &counts, &reentry, false);
    let generation = RuntimeGeneration::new(2).unwrap();
    let (arena, _) = source(generation, &counts, &reentry, false);
    let runtime = ProbeRuntime::with_host(
        generation,
        RtdLimits::standard(),
        RtdSubscriptionHost::detached(),
        arena,
    );
    assert!(matches!(
        runtime.prepare(
            &old_handle,
            RtdTopic::single("stale-source").unwrap().borrowed()
        ),
        Err(XllError::StaleHandle)
    ));
    assert_eq!(counts.subscribed.load(Ordering::Acquire), 0);
    drop(old_arena);
    runtime.close().unwrap();
}

#[derive(Clone, Copy)]
enum DisconnectFailure {
    None,
    Error,
    Panic,
}

struct FailingSubscription {
    counts: Arc<Counts>,
    failure: DisconnectFailure,
}

// SAFETY: the fixture owns no sink and starts no work.
unsafe impl RtdSubscription for FailingSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self) -> XllResult<()> {
        self.counts.disconnected.fetch_add(1, Ordering::Relaxed);
        match self.failure {
            DisconnectFailure::None => Ok(()),
            DisconnectFailure::Error => Err(XllError::Overloaded),
            DisconnectFailure::Panic => panic!("injected by-value owner disconnect panic"),
        }
    }
}

impl Drop for FailingSubscription {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn failing_owner(
    counts: &Arc<Counts>,
    failure: DisconnectFailure,
) -> SourceSubscription<FailingSubscription> {
    SourceSubscription {
        subscription: Some(FailingSubscription {
            counts: Arc::clone(counts),
            failure,
        }),
        _not_sync: PhantomData,
    }
}

#[test]
fn miri_by_value_source_owner_drop_contains_error_and_panic_once() {
    for failure in [
        DisconnectFailure::None,
        DisconnectFailure::Error,
        DisconnectFailure::Panic,
    ] {
        let counts = Arc::new(Counts::default());
        let owner = failing_owner(&counts, failure);
        assert!(catch_no_unwind(AssertUnwindSafe(|| drop(owner))).is_ok());
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn miri_by_value_source_owner_consuming_barrier_never_retries() {
    for failure in [
        DisconnectFailure::None,
        DisconnectFailure::Error,
        DisconnectFailure::Panic,
    ] {
        let counts = Arc::new(Counts::default());
        let owner = failing_owner(&counts, failure);
        let result = catch_no_unwind(AssertUnwindSafe(|| owner.disconnect_and_wait()));
        match failure {
            DisconnectFailure::None => assert!(matches!(result, Ok(Ok(())))),
            DisconnectFailure::Error => {
                assert!(matches!(result, Ok(Err(XllError::Overloaded))));
            }
            DisconnectFailure::Panic => assert!(result.is_err()),
        }
        assert_eq!(counts.disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(counts.dropped.load(Ordering::Relaxed), 1);
    }
}

struct StaticAddressSource {
    disconnected: AtomicUsize,
}
struct StaticAddressSubscription {
    disconnected: NonNull<AtomicUsize>,
}
// SAFETY: the pointer identifies an atomic in the catalog allocation, which
// runtime admission keeps alive until this unique subscription is consumed.
unsafe impl Send for StaticAddressSubscription {}
// SAFETY: the fixture retains no sink or worker; consumption touches the
// stable source only synchronously and then discards its non-owning pointer.
unsafe impl RtdSubscription for StaticAddressSubscription {
    fn request_cancel(&self) {}
    fn disconnect_and_wait(self) -> XllResult<()> {
        // SAFETY: SourceSubscription's caller retains the source allocation
        // through this barrier; this test deliberately moves only its owner.
        unsafe { self.disconnected.as_ref() }.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}
// SAFETY: the supplied sink is discarded and no background execution escapes.
// The subscription pointer is covered by the source allocation lifetime proof.
unsafe impl RtdSource for StaticAddressSource {
    type Value = i32;
    type Subscription = StaticAddressSubscription;
    fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<Self::Subscription> {
        Ok(StaticAddressSubscription {
            disconnected: NonNull::from(&self.disconnected),
        })
    }
}

#[test]
fn miri_static_catalog_subscription_retains_source_address_across_owner_moves() {
    type Runtime = crate::subscription::runtime::SubscriptionRuntime<
        RtdSubscriptionHost,
        StaticAddressSubscription,
        (StaticAddressSource,),
    >;
    let registration = SourceRegistration::<StaticAddressSubscription, (StaticAddressSource,)>::new(
        RuntimeGeneration::new(1).unwrap(),
    );
    let (handle,) = registration
        .register_sources((StaticAddressSource {
            disconnected: AtomicUsize::new(0),
        },))
        .unwrap();
    let arena = registration.finish();
    let publish_runtime = Box::new(ProbeRuntime::new());
    let server = publish_runtime.register_test_server(1);
    let source = arena.resolve(handle.id).unwrap();
    let address = NonNull::from(&source.catalog.0.disconnected);
    let topic = RtdTopic::single("static owner relocation").unwrap();
    // SAFETY: the boxed tuple allocation and independent publish root outlive
    // this owner even while their containing SourceArena and Runtime move.
    let subscription =
        unsafe { SourceSubscription::subscribe(&source, &topic, sink(&server)) }.unwrap();
    let moved = Box::new(arena);
    let mut runtimes = Vec::with_capacity(1);
    runtimes.push(Runtime::with_sources_for_internal(*moved));
    for _ in 0..3 {
        runtimes.push(Runtime::new());
    }
    let source = runtimes[0].sources.resolve(handle.id).unwrap();
    assert_eq!(NonNull::from(&source.catalog.0.disconnected), address);
    subscription.disconnect_and_wait().unwrap();
    assert_eq!(source.catalog.0.disconnected.load(Ordering::Acquire), 1);
    for runtime in &runtimes {
        runtime.close().unwrap();
    }
}

struct ConversionBarrierSubscription(Arc<AtomicUsize>);
impl Drop for ConversionBarrierSubscription {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}
// SAFETY: there is no retained sink or worker; Drop completes this fixture's
// ownership barrier, including conversion and cancellation unwind paths.
unsafe impl RtdSubscription for ConversionBarrierSubscription {
    fn request_cancel(&self) {}
    fn disconnect_and_wait(self) -> XllResult<()> {
        Ok(())
    }
}
// SAFETY: unwinding drops self, completing its no-worker ownership barrier.
unsafe impl IntoRtdSubscription<()> for ConversionBarrierSubscription {
    fn into_rtd_subscription(self) {
        panic!("injected subscription conversion panic");
    }
}
struct ConversionBarrierSource(Arc<AtomicUsize>);
// SAFETY: subscribe discards its sink and transfers the sole barrier owner.
unsafe impl RtdSource for ConversionBarrierSource {
    type Value = i32;
    type Subscription = ConversionBarrierSubscription;
    fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<Self::Subscription> {
        Ok(ConversionBarrierSubscription(Arc::clone(&self.0)))
    }
}
#[test]
fn miri_static_catalog_conversion_panic_preserves_subscription_barrier() {
    let completed = Arc::new(AtomicUsize::new(0));
    let registration = SourceRegistration::<(), (ConversionBarrierSource,)>::new(
        RuntimeGeneration::new(1).unwrap(),
    );
    let (handle,) = registration
        .register_sources((ConversionBarrierSource(Arc::clone(&completed)),))
        .unwrap();
    let runtime =
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish());
    let server = runtime.register_test_server(1);
    let prepared = runtime
        .prepare(
            &handle,
            RtdTopic::single("conversion panic").unwrap().borrowed(),
        )
        .unwrap();
    assert!(
        catch_no_unwind(AssertUnwindSafe(|| runtime.connect_transaction(
            &server,
            TopicId(1),
            prepared.id()
        )))
        .is_err()
    );
    prepared.rollback();
    assert_eq!(completed.load(Ordering::Acquire), 1);
    assert!(runtime.close().is_err());
    assert_eq!(completed.load(Ordering::Acquire), 1);
}
