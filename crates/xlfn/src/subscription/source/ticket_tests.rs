use super::*;
use crate::excel_rtd::RtdSubscriptionHost;
use crate::generation::ConnectionGeneration;
use crate::subscription::host::SubscriptionHost;
use crate::subscription::{RtdLimits, SubscriptionRuntime, TopicId};
use std::cell::Cell;
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
static_assertions::assert_impl_all!(SourceSubscription: Send);
static_assertions::assert_not_impl_any!(SourceSubscription: Sync, Clone, Copy);

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
) -> (SourceArena, RtdSourceHandle<ProbeSource>) {
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

fn sink(
    server: &crate::subscription::server::SubscriptionServerHandle<RtdSubscriptionHost>,
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
    let publish_runtime = Box::new(SubscriptionRuntime::new());
    let server = publish_runtime.register_test_server(1);
    let adapter = arena.resolve(handle.id).unwrap();
    let adapter_address = NonNull::from(adapter);
    let topic = RtdTopic::single("move-source-owner").unwrap();
    let mut tickets = Vec::new();
    for _ in 0..65 {
        // SAFETY: the arena's boxed source allocation and independent publish
        // root outlive every ticket, including the scoped worker below.
        tickets.push(
            unsafe { SourceSubscription::subscribe(adapter, &topic, sink(&server)) }.unwrap(),
        );
    }

    let moved_arena = Box::new(arena);
    let mut runtimes = Vec::with_capacity(1);
    runtimes.push(SubscriptionRuntime::with_sources_for_internal(*moved_arena));
    for _ in 0..3 {
        // Force relocation of the owning runtime's Vec allocation. No raw
        // server handle has ever been created for these movable runtimes.
        runtimes.push(SubscriptionRuntime::new());
    }
    assert_eq!(
        NonNull::from(runtimes[0].sources.resolve(handle.id).unwrap()),
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
fn miri_source_ticket_cancel_restores_after_panic_and_reuses_new_generation() {
    let counts = Arc::new(Counts::default());
    let reentry = Arc::new(Mutex::new(None));
    let (arena, handle) = source(RuntimeGeneration::new(1).unwrap(), &counts, &reentry, true);
    let runtime = Box::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime.register_test_server(1);
    let adapter = runtime.sources.resolve(handle.id).unwrap();
    let topic = RtdTopic::single("cancel-restore").unwrap();
    // SAFETY: this fixed runtime and publish root remain alive until both
    // tickets have been consumed, including cancellation's unwind path.
    let mut ticket =
        unsafe { SourceSubscription::subscribe(adapter, &topic, sink(&server)) }.unwrap();
    let old_key = ticket.key.unwrap();
    assert!(catch_no_unwind(AssertUnwindSafe(|| ticket.request_cancel())).is_err());
    ticket.disconnect_and_wait().unwrap();
    // SAFETY: same fixed source/publish allocations as the previous ticket.
    let reused = unsafe { SourceSubscription::subscribe(adapter, &topic, sink(&server)) }.unwrap();
    assert_ne!(old_key, reused.key.unwrap());
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
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime.register_test_server(1);
    let topic = RtdTopic::single("outer-cancel").unwrap();
    // SAFETY: the Arc below retains the runtime through this unique ticket's
    // consumption. No callback may outlive the synchronous test hook.
    let mut ticket = unsafe {
        SourceSubscription::subscribe(
            runtime.sources.resolve(handle.id).unwrap(),
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
    let runtime = SubscriptionRuntime::with_host(
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
