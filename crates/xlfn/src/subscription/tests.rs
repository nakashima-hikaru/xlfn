#![allow(
    unsafe_code,
    reason = "RTD tests implement the audited non-owning subscription lifetime contract"
)]

use super::*;
use crate::excel_rtd::{RtdNotifier, RtdSubscriptionHost};
use crate::rtd::test_support::{TestNotifierState, TestNotifyOutcome};

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct TestSubscription {
    canceled: Arc<AtomicBool>,
    disconnected: Arc<AtomicBool>,
}

// SAFETY: test subscription does not access external resources.
unsafe impl RtdSubscription for TestSubscription {
    fn request_cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.disconnected.store(true, Ordering::Release);
        Ok(())
    }
}

pub(crate) struct PublishingSource<T = RtdValue, F = fn() -> XllResult<()>> {
    initial: Option<T>,
    sink_slot: Arc<Mutex<Option<RtdSink<T>>>>,
    canceled: Arc<AtomicBool>,
    disconnected: Arc<AtomicBool>,
    on_subscribe: Option<F>,
}

impl<T, F> RtdSource for PublishingSource<T, F>
where
    T: IntoRtdValue + Clone + Send + Sync + 'static,
    F: Fn() -> XllResult<()> + Send + Sync + 'static,
{
    type Value = T;
    type Subscription = TestSubscription;

    fn subscribe(
        &self,
        _topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription> {
        if let Some(on_sub) = &self.on_subscribe {
            on_sub()?;
        }
        if let Some(initial) = self.initial.clone() {
            sink.publish(initial)?;
        }
        *self.sink_slot.lock() = Some(sink);
        Ok(TestSubscription {
            canceled: Arc::clone(&self.canceled),
            disconnected: Arc::clone(&self.disconnected),
        })
    }
}

pub(crate) struct SourceFixture {
    registration: SourceRegistration,
}

impl SourceFixture {
    pub(crate) fn new() -> Self {
        Self {
            registration: SourceRegistration::new(
                crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
            ),
        }
    }

    #[allow(
        clippy::type_complexity,
        reason = "test helper returns a tuple of handles and sinks"
    )]
    pub(crate) fn add<T: IntoRtdValue + Clone + Send + Sync + 'static>(
        &self,
        initial: Option<T>,
    ) -> (
        RtdSourceHandle<PublishingSource<T, fn() -> XllResult<()>>>,
        Arc<Mutex<Option<RtdSink<T>>>>,
        Arc<AtomicBool>,
    ) {
        let slot = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = self
            .registration
            .register(PublishingSource {
                initial,
                sink_slot: Arc::clone(&slot),
                canceled: Arc::new(AtomicBool::new(false)),
                disconnected: Arc::clone(&disconnected),
                on_subscribe: None,
            })
            .expect("test source handle allocation must succeed");
        (source, slot, disconnected)
    }

    pub(crate) fn finish(self) -> SourceArena {
        self.registration.finish()
    }
}

pub(crate) type PublishingSourceResult<T> = (
    SourceArena,
    RtdSourceHandle<PublishingSource<T, fn() -> XllResult<()>>>,
    Arc<Mutex<Option<RtdSink<T>>>>,
    Arc<AtomicBool>,
);

pub(crate) fn publishing_source<T: IntoRtdValue + Clone + Send + Sync + 'static>(
    initial: Option<T>,
) -> PublishingSourceResult<T> {
    let fixture = SourceFixture::new();
    let (source, slot, disconnected) = fixture.add(initial);
    (fixture.finish(), source, slot, disconnected)
}

fn connected_sink<T: IntoRtdValue + Clone + Send + Sync + 'static>(
    initial: Option<T>,
    topic: &str,
) -> (
    Arc<SubscriptionRuntime>,
    SubscriptionServerHandle,
    RtdSink<T>,
) {
    let (arena, source, sink_slot, _) = publishing_source(initial);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single(topic).unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink_slot.lock().clone().expect("source must capture sink");
    (runtime, server, sink)
}

#[test]
fn rtd_capacity_distinguishes_disabled_and_bounded_limits() {
    assert_eq!(RtdCapacity::from_usize(0), RtdCapacity::Disabled);
    assert!(RtdCapacity::disabled().is_disabled());

    let bounded = RtdCapacity::from_usize(4);
    assert_eq!(
        bounded,
        RtdCapacity::Bounded(NonZeroUsize::new(4).expect("test limit is non-zero"))
    );
    assert_eq!(bounded.get(), 4);
    assert!(!bounded.is_disabled());
}

#[test]
fn server_publish_isolation() {
    let fixture = SourceFixture::new();
    let (source_a, _sink_a, _) = fixture.add(Some(1.0f64));
    let (source_b, sink_b, _) = fixture.add(Some(2.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a").unwrap())
        .unwrap();
    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b").unwrap())
        .unwrap();

    let id_a = prep_a.id();
    let id_b = prep_b.id();
    prep_a.commit();
    prep_b.commit();

    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink_b = sink_b.lock().clone().unwrap();

    let lock_guard = server_a.test_server().publish.lock_shard_for_test(0);

    let b_published = Arc::new(AtomicBool::new(false));
    let b_published_clone = Arc::clone(&b_published);
    let handle_b = std::thread::spawn(move || {
        sink_b.publish(100.0).unwrap();
        b_published_clone.store(true, Ordering::Release);
    });

    handle_b.join().unwrap();
    assert!(b_published.load(Ordering::Acquire));
    drop(lock_guard);
}

#[test]
fn notification_callback_isolation() {
    let fixture = SourceFixture::new();
    let (source_a, sink_a, _) = fixture.add(Some(1.0f64));
    let (source_b, sink_b, _) = fixture.add(Some(2.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let state_a = Arc::new(TestNotifierState::default());
    *state_a.entered.lock() = Some(entered_tx);
    *state_a.release.lock() = Some(release_rx);

    server_a
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state_a)))
        .unwrap();

    let state_b = Arc::new(TestNotifierState::default());
    server_b
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state_b)))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a").unwrap())
        .unwrap();
    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    let id_b = prep_b.id();
    prep_a.commit();
    prep_b.commit();

    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink_a = sink_a.lock().clone().unwrap();
    let sink_b = sink_b.lock().clone().unwrap();

    let thread_a = std::thread::spawn(move || {
        sink_a.publish(10.0).unwrap();
    });

    entered_rx.recv().unwrap();

    for i in 0..100 {
        sink_b.publish(i as f64).unwrap();
    }

    assert!(state_b.calls.load(Ordering::SeqCst) > 0);

    release_tx.send(()).unwrap();
    thread_a.join().unwrap();
}

#[test]
fn server_locality_refresh_lock_independence() {
    let (arena, source_b, sink_b, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b-0").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink_b = sink_b.lock().clone().unwrap();
    sink_b.publish(42.0).unwrap();

    // server A の shard mutex を保持した状態で server B.begin_refresh を実行
    let _guard_a = server_a.test_server().publish.lock_shard_for_test(0);

    let (tx, rx) = std::sync::mpsc::channel();
    let server_b_clone = server_b;
    let handle = std::thread::spawn(move || {
        let batch = server_b_clone.begin_refresh().unwrap();
        let observed = (batch.updates.len(), batch.updates[0].value.clone());
        batch.complete(RefreshOutcome::Delivered).unwrap();
        tx.send(observed).unwrap();
    });

    let (update_count, value) = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("server_b.begin_refresh should not block on server_a state lock");

    assert_eq!(update_count, 1);
    assert_eq!(value, StoredRtdValue::Number(42.0));
    handle.join().unwrap();
}

#[test]
fn refresh_batch_borrows_server_lifetime() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let batch = server.begin_refresh().unwrap();
    assert!(batch.updates.is_empty());
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn runtime_close_blocks_all_servers_immediately() {
    let fixture = SourceFixture::new();
    let (source_b, sink_b, _) = fixture.add(Some(0.0f64));
    let (source_a, sink_a, _) = fixture.add(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b-0").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let (callback_started_tx, callback_started_rx) = std::sync::mpsc::channel();
    let (unblock_callback_tx, unblock_callback_rx) = std::sync::mpsc::channel();

    let state_a = Arc::new(TestNotifierState::default());
    *state_a.entered.lock() = Some(callback_started_tx);
    *state_a.release.lock() = Some(unblock_callback_rx);

    server_a
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state_a)))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a-0").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    prep_a.commit();
    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();

    let sink_a = sink_a.lock().clone().unwrap();
    let publish_handle = std::thread::spawn(move || {
        sink_a.publish(1.0).unwrap();
    });

    callback_started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();

    let runtime_clone = Arc::clone(&runtime);
    let close_handle = std::thread::spawn(move || {
        runtime_clone.close().unwrap();
    });

    // server_a が close 処理に入り OperationGate が Closing になるまで待機
    while server_a.test_server().enter_operation().is_ok() {
        std::thread::yield_now();
    }

    let sink_b = sink_b.lock().clone().unwrap();
    assert!(matches!(sink_b.publish(42.0), Err(XllError::Closing)));

    unblock_callback_tx.send(()).unwrap();
    publish_handle.join().unwrap();
    close_handle.join().unwrap();
}

#[test]
fn server_termination_clears_pending_and_allows_reconnect() {
    let (arena, source, sink, disconnected) = publishing_source(Some(10.0));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("shared-topic").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id)
        .unwrap();
    conn_a.commit().unwrap();

    // server A を終了
    server_a.terminate().unwrap();

    // disconnect が呼ばれていること
    assert!(disconnected.load(Ordering::SeqCst));
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();
    let prep_b = runtime
        .prepare(&source, RtdTopic::single("shared-topic").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();

    runtime.claim_server(server_b.generation(), id_b).unwrap();

    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink = sink.lock().clone().unwrap();
    sink.publish(20.0).unwrap();

    let batch = server_b.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].value, StoredRtdValue::Number(20.0));
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn uncommitted_update_does_not_trigger_notification() {
    let fixture = SourceFixture::new();
    let (source_a, _, _) = fixture.add(Some(0.0f64));
    let (source_b, sink_b, _) = fixture.add(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a-0").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    prep_a.commit();
    let conn_a = runtime
        .connect_transaction(&server, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();

    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b-0").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();
    let _conn_b = runtime
        .connect_transaction(&server, TopicId(2), id_b)
        .unwrap();

    state.calls.store(0, Ordering::SeqCst);

    let sink_b = sink_b.lock().clone().unwrap();
    sink_b.publish(100.0).unwrap();

    server.pulse_notification().unwrap();

    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn publish_between_install_and_commit_prepares_notification() {
    let (arena, source, sink, _) = publishing_source(Some(1.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let prepared = runtime
        .prepare(&source, RtdTopic::single("known-update").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();

    let connection = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();

    let sink = sink.lock().clone().unwrap();
    sink.publish(2.0).unwrap();

    connection.commit().unwrap();

    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn deliverable_pending_accounting_tracks_connection_lifecycle() {
    let (arena, source, sink, _) = publishing_source(Some(1.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prepared = runtime
        .prepare(&source, RtdTopic::single("accounting").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();

    let connection = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    // The source's initial publish and this update both belong to an
    // uncommitted connection, so neither is deliverable yet.
    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 0);
    sink.publish(2.0).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 0);

    connection.commit().unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    batch.complete(RefreshOutcome::Delivered).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    assert_eq!(server.pending_update_count(), 0);

    sink.publish(3.0).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    runtime.disconnect(&server, TopicId(1)).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    assert_eq!(server.pending_update_count(), 0);
}

#[test]
fn first_empty_publish_is_deliverable() {
    let (_runtime, server, sink) = connected_sink::<()>(None, "first-empty-publish");

    sink.publish(()).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].value, StoredRtdValue::Empty);
    batch.complete(RefreshOutcome::Delivered).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 0);
}

#[test]
fn repeated_empty_publish_is_suppressed() {
    let (_runtime, server, sink) = connected_sink::<()>(None, "repeated-empty-publish");

    sink.publish(()).unwrap();
    sink.publish(()).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].sequence, 0);
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn repeated_number_publish_is_suppressed() {
    let (_runtime, server, sink) = connected_sink::<f64>(None, "repeated-number-publish");

    sink.publish(12.5).unwrap();
    sink.publish(12.5).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].value, StoredRtdValue::Number(12.5));
    assert_eq!(batch.updates[0].sequence, 0);
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn repeated_string_publish_is_suppressed() {
    let (_runtime, server, sink) = connected_sink::<String>(None, "repeated-string-publish");

    sink.publish("same-value".to_owned()).unwrap();
    sink.publish("same-value".to_owned()).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(
        batch.updates[0].value,
        StoredRtdValue::String("same-value".into()),
    );
    assert_eq!(batch.updates[0].sequence, 0);
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn changed_value_after_same_value_is_delivered() {
    let (_runtime, server, sink) = connected_sink::<f64>(None, "changed-after-same");

    sink.publish(10.0).unwrap();
    let first = server.begin_refresh().unwrap();
    assert_eq!(first.updates.len(), 1);
    first.complete(RefreshOutcome::Delivered).unwrap();

    sink.publish(10.0).unwrap();
    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    sink.publish(11.0).unwrap();

    let changed = server.begin_refresh().unwrap();
    assert_eq!(changed.updates.len(), 1);
    assert_eq!(changed.updates[0].value, StoredRtdValue::Number(11.0));
    changed.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn same_value_while_pending_does_not_allocate_new_sequence() {
    let (_runtime, server, sink) = connected_sink::<f64>(None, "same-while-pending");

    sink.publish(7.0).unwrap();
    sink.publish(7.0).unwrap();

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].sequence, 0);
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn same_value_after_successful_refresh_is_suppressed() {
    let (_runtime, server, sink) = connected_sink::<f64>(None, "same-after-refresh");

    sink.publish(21.0).unwrap();
    let first = server.begin_refresh().unwrap();
    first.complete(RefreshOutcome::Delivered).unwrap();

    sink.publish(21.0).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    assert_eq!(server.pending_update_count(), 0);
    let empty = server.begin_refresh().unwrap();
    assert!(empty.updates.is_empty());
    empty.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn reconnect_does_not_inherit_previous_generation_latest() {
    let (arena, source, sink_slot, _) = publishing_source(Some(100.0_f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prepared_a = runtime
        .prepare(&source, RtdTopic::single("generation-latest").unwrap())
        .unwrap();
    let id_a = prepared_a.id();
    prepared_a.commit();
    let connection_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    assert_eq!(connection_a.value(), &StoredRtdValue::Number(100.0));
    connection_a.commit().unwrap();

    server_a.terminate().unwrap();

    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();
    let prepared_b = runtime
        .prepare(&source, RtdTopic::single("generation-latest").unwrap())
        .unwrap();
    let id_b = prepared_b.id();
    prepared_b.commit();
    runtime.claim_server(server_b.generation(), id_b).unwrap();
    let connection_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    assert_eq!(connection_b.value(), &StoredRtdValue::Number(100.0));
    connection_b.commit().unwrap();

    assert_eq!(server_b.test_server().publish.queued_update_count(), 0);
    let sink = sink_slot.lock().clone().expect("reconnected source sink");
    sink.publish(100.0).unwrap();
    assert_eq!(server_b.test_server().publish.queued_update_count(), 0);
    let second = server_b.begin_refresh().unwrap();
    assert!(second.updates.is_empty());
    second.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn old_buffer_update_is_not_redelivered_after_newer_update() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("superseded").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    sink.publish(10.0).unwrap();
    let first = server.begin_refresh().unwrap();
    first.complete(RefreshOutcome::Failed).unwrap();

    sink.publish(11.0).unwrap();
    let latest = server.begin_refresh().unwrap();
    assert_eq!(latest.updates.len(), 1);
    assert_eq!(latest.updates[0].value, StoredRtdValue::Number(11.0));
    latest.complete(RefreshOutcome::Delivered).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    assert_eq!(server.pending_update_count(), 0);
    let empty = server.begin_refresh().unwrap();
    assert!(empty.updates.is_empty());
    empty.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn two_buffer_string_refresh_picks_newer_sequence_and_cleans_both() {
    let (arena, source, sink, _) = publishing_source::<String>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("string-superseded").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    sink.publish("first-update".to_string()).unwrap();
    let first = server.begin_refresh().unwrap();
    first.complete(RefreshOutcome::Failed).unwrap();

    sink.publish("second-update".to_string()).unwrap();
    let latest = server.begin_refresh().unwrap();
    assert_eq!(latest.updates.len(), 1);
    assert_eq!(
        latest.updates[0].value,
        StoredRtdValue::String("second-update".into())
    );
    latest.complete(RefreshOutcome::Delivered).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 0);
    assert_eq!(server.pending_update_count(), 0);
    let empty = server.begin_refresh().unwrap();
    assert!(empty.updates.is_empty());
    empty.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn newer_update_survives_completion_of_older_refresh() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("newer-survives").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    sink.publish(10.0).unwrap();
    let older = server.begin_refresh().unwrap();
    sink.publish(11.0).unwrap();
    older.complete(RefreshOutcome::Delivered).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let newer = server.begin_refresh().unwrap();
    assert_eq!(newer.updates.len(), 1);
    assert_eq!(newer.updates[0].value, StoredRtdValue::Number(11.0));
    newer.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn failed_refresh_keeps_pending_update() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("failed-refresh").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    sink.publish(10.0).unwrap();
    let failed = server.begin_refresh().unwrap();
    let sequence = failed.updates[0].sequence;
    failed.complete(RefreshOutcome::Failed).unwrap();

    assert_eq!(server.test_server().publish.queued_update_count(), 1);
    assert_eq!(server.pending_update_count(), 1);
    let retry = server.begin_refresh().unwrap();
    assert_eq!(retry.updates.len(), 1);
    assert_eq!(retry.updates[0].sequence, sequence);
    retry.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn concurrent_publish_after_refresh_snapshot_is_delivered_later() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("concurrent-publish").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    let sink = sink.lock().clone().unwrap();

    sink.publish(10.0).unwrap();
    let snapshot = server.begin_refresh().unwrap();
    let concurrent_sink = sink.clone();
    std::thread::spawn(move || concurrent_sink.publish(11.0).unwrap())
        .join()
        .unwrap();
    snapshot.complete(RefreshOutcome::Delivered).unwrap();

    let later = server.begin_refresh().unwrap();
    assert_eq!(later.updates.len(), 1);
    assert_eq!(later.updates[0].value, StoredRtdValue::Number(11.0));
    later.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn refresh_collection_skips_shards_without_deliverable_updates() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("ready-shard").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    sink.lock().clone().unwrap().publish(10.0).unwrap();

    let unrelated_shard = server.test_server().publish.lock_shard_for_test(0);
    let server_clone = server;
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let batch = server_clone.begin_refresh().unwrap();
        let count = batch.updates.len();
        batch.complete(RefreshOutcome::Delivered).unwrap();
        tx.send(count).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("refresh must not traverse a shard absent from its ready index"),
        1,
    );
    drop(unrelated_shard);
    handle.join().unwrap();
}

#[test]
fn refresh_planning_does_not_traverse_topic_shards() {
    let (arena, source, sink, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prepared = runtime
        .prepare(&source, RtdTopic::single("planning-only").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    sink.lock().clone().unwrap().publish(10.0).unwrap();

    let ready_shard = server.test_server().publish.lock_shard_for_test(1);
    let planned = server.test_server().publish.plan_refresh().unwrap();
    drop(ready_shard);
    drop(planned);
}

#[test]
fn refresh_preserves_latest_update_for_each_topic() {
    let fixture = SourceFixture::new();
    let (source_one, sink_one, _) = fixture.add::<f64>(None);
    let (source_two, sink_two, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    for (source, topic_id, name) in [
        (&source_one, TopicId(1), "reduction-one"),
        (&source_two, TopicId(2), "reduction-two"),
    ] {
        let prepared = runtime
            .prepare(source, RtdTopic::single(name).unwrap())
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, topic_id, id)
            .unwrap()
            .commit()
            .unwrap();
    }

    sink_two.lock().clone().unwrap().publish(20.0).unwrap();
    sink_one.lock().clone().unwrap().publish(10.0).unwrap();

    let batch = server.begin_refresh().unwrap();
    let mut actual = batch
        .updates
        .iter()
        .map(|update| (update.topic_id, update.value.clone()))
        .collect::<Vec<_>>();
    actual.sort_by_key(|(topic_id, _)| *topic_id);
    assert_eq!(
        actual,
        vec![
            (1, StoredRtdValue::Number(10.0)),
            (2, StoredRtdValue::Number(20.0)),
        ]
    );
    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn server_standalone_termination() {
    let (arena, source_b, sink_b, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink_b = sink_b.lock().clone().unwrap();

    let server_a_clone = server_a;
    let handle_term = std::thread::spawn(move || {
        server_a_clone.terminate().unwrap();
    });

    sink_b.publish(1.0).unwrap();
    let batch = server_b.begin_refresh().unwrap();
    batch.complete(RefreshOutcome::Delivered).unwrap();

    handle_term.join().unwrap();
}

#[test]
fn stale_sink_returns_closing() {
    let fixture = SourceFixture::new();
    let (source_a, sink_a, _) = fixture.add(Some(0.0f64));
    let (source_b, sink_b, _) = fixture.add(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    prep_a.commit();
    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();

    let sink_a = sink_a.lock().clone().unwrap();

    server_a.terminate().unwrap();

    assert!(matches!(sink_a.publish(1.0), Err(XllError::Closing)));

    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("b").unwrap())
        .unwrap();
    let id_b = prep_b.id();
    prep_b.commit();
    let conn_b = runtime
        .connect_transaction(&server_b, TopicId(1), id_b)
        .unwrap();
    conn_b.commit().unwrap();

    let sink_b = sink_b.lock().clone().unwrap();
    assert!(sink_b.publish(2.0).is_ok());
}

#[test]
fn global_quota_enforcement() {
    let limits = RtdLimits {
        max_queued_updates: RtdCapacity::from_usize(10),
        ..RtdLimits::standard()
    };
    let fixture = SourceFixture::new();
    let mut sources_a = Vec::new();
    for _ in 0..6 {
        sources_a.push(fixture.add(Some(0.0f64)));
    }
    let mut sources_b = Vec::new();
    for _ in 0..5 {
        sources_b.push(fixture.add(Some(0.0f64)));
    }
    let runtime = Arc::new(SubscriptionRuntime::with_host(
        RuntimeGeneration::new(1).expect("test generation is non-zero"),
        limits,
        RtdSubscriptionHost::default(),
        fixture.finish(),
    ));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let mut sinks_a = Vec::new();
    for (i, (source, sink, _)) in sources_a.into_iter().enumerate() {
        let prep = runtime
            .prepare(&source, RtdTopic::single(format!("a-{}", i)).unwrap())
            .unwrap();
        let id = prep.id();
        prep.commit();
        let conn = runtime
            .connect_transaction(&server_a, TopicId(i as i32), id)
            .unwrap();
        conn.commit().unwrap();
        sinks_a.push(sink.lock().clone().unwrap());
    }

    let mut sinks_b = Vec::new();
    for (i, (source, sink, _)) in sources_b.into_iter().enumerate() {
        let prep = runtime
            .prepare(&source, RtdTopic::single(format!("b-{}", i)).unwrap())
            .unwrap();
        let id = prep.id();
        prep.commit();
        let conn = runtime
            .connect_transaction(&server_b, TopicId(i as i32), id)
            .unwrap();
        conn.commit().unwrap();
        sinks_b.push(sink.lock().clone().unwrap());
    }

    for sink in &sinks_a[0..6] {
        sink.publish(1.0).unwrap();
    }
    for sink in &sinks_b[0..4] {
        sink.publish(1.0).unwrap();
    }

    assert!(matches!(sinks_b[4].publish(1.0), Err(XllError::Overloaded)));

    let batch_a = server_a.begin_refresh().unwrap();
    batch_a.complete(RefreshOutcome::Delivered).unwrap();

    assert!(sinks_b[4].publish(1.0).is_ok());
}

#[test]
fn key_binding_concurrency_rejection() {
    let (arena, source, _, _) = publishing_source(Some(1.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("shared").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id)
        .unwrap();
    assert!(matches!(
        runtime.connect_transaction(&server_b, TopicId(1), id),
        Err(XllError::Internal { .. })
    ));

    conn_a.commit().unwrap();
    assert!(matches!(
        runtime.connect_transaction(&server_b, TopicId(1), id),
        Err(XllError::Internal { .. })
    ));
}

#[test]
fn runtime_close_waits_for_inflight() {
    let (arena, source_a, sink_a, _) = publishing_source(Some(1.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let _server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let state_a = Arc::new(TestNotifierState::default());
    *state_a.entered.lock() = Some(entered_tx);
    *state_a.release.lock() = Some(release_rx);

    server_a
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state_a)))
        .unwrap();

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    prep_a.commit();
    let conn_a = runtime
        .connect_transaction(&server_a, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();

    let sink_a = sink_a.lock().clone().unwrap();

    let handle_a = std::thread::spawn(move || {
        sink_a.publish(10.0).unwrap();
    });

    entered_rx.recv().unwrap();

    let runtime_clone = Arc::clone(&runtime);
    let closed_flag = Arc::new(AtomicBool::new(false));
    let cf = Arc::clone(&closed_flag);

    let handle_close = std::thread::spawn(move || {
        runtime_clone.close().unwrap();
        cf.store(true, Ordering::Release);
    });

    while !runtime.runtime_gate.is_closing() {
        std::thread::yield_now();
    }
    assert!(!closed_flag.load(Ordering::Acquire));

    release_tx.send(()).unwrap();
    handle_a.join().unwrap();
    handle_close.join().unwrap();
    assert!(closed_flag.load(Ordering::Acquire));
}

#[test]
fn inflight_register_waits_for_close() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (enter_tx, enter_rx) = std::sync::mpsc::channel();
    let (unblock_tx, unblock_rx) = std::sync::mpsc::channel();
    let unblock_rx = Arc::new(Mutex::new(unblock_rx));

    runtime.set_operation_enter_hook(Some(Arc::new(move || {
        enter_tx.send(()).unwrap();
        unblock_rx.lock().recv().unwrap();
    })));

    let runtime_clone = Arc::clone(&runtime);
    let handle_reg = std::thread::spawn(move || {
        runtime_clone
            .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
    });

    enter_rx.recv().unwrap();

    let runtime_close = Arc::clone(&runtime);
    let closed_flag = Arc::new(AtomicBool::new(false));
    let cf = Arc::clone(&closed_flag);
    let handle_close = std::thread::spawn(move || {
        runtime_close.close().unwrap();
        cf.store(true, Ordering::Release);
    });

    while !runtime.runtime_gate.is_closing() {
        std::thread::yield_now();
    }
    assert!(!closed_flag.load(Ordering::Acquire));

    unblock_tx.send(()).unwrap();

    let reg_res = handle_reg.join().unwrap();
    handle_close.join().unwrap();

    assert!(closed_flag.load(Ordering::Acquire));
    let server = reg_res.unwrap();
    assert!(matches!(
        server.test_server().enter_operation(),
        Err(XllError::Closing)
    ));
}

#[test]
fn inflight_prepare_waits_for_close() {
    let (enter_tx, enter_rx) = std::sync::mpsc::channel();
    let (unblock_tx, unblock_rx) = std::sync::mpsc::channel();
    let unblock_rx = Arc::new(Mutex::new(unblock_rx));

    let source_dropped = Arc::new(AtomicBool::new(false));
    struct DroppingSource(Arc<AtomicBool>);
    impl Drop for DroppingSource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    impl RtdSource for DroppingSource {
        type Value = RtdValue;
        type Subscription = TestSubscription;
        fn subscribe(
            &self,
            _: &RtdTopic,
            _: RtdSink<Self::Value>,
        ) -> XllResult<Self::Subscription> {
            Ok(TestSubscription {
                canceled: Arc::new(AtomicBool::new(false)),
                disconnected: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    let (arena, source) = SourceArena::with_source(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        DroppingSource(Arc::clone(&source_dropped)),
    )
    .unwrap();
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));

    runtime.set_operation_enter_hook(Some(Arc::new(move || {
        enter_tx.send(()).unwrap();
        unblock_rx.lock().recv().unwrap();
    })));

    let runtime_clone = Arc::clone(&runtime);
    let handle_prep = std::thread::spawn(move || {
        let prep = runtime_clone.prepare(&source, RtdTopic::single("topic").unwrap())?;
        drop(prep);
        Ok::<(), XllError>(())
    });

    enter_rx.recv().unwrap();

    let runtime_close = Arc::clone(&runtime);
    let closed_flag = Arc::new(AtomicBool::new(false));
    let cf = Arc::clone(&closed_flag);
    let handle_close = std::thread::spawn(move || {
        runtime_close.close().unwrap();
        cf.store(true, Ordering::Release);
    });

    while !runtime.runtime_gate.is_closing() {
        std::thread::yield_now();
    }
    assert!(!closed_flag.load(Ordering::Acquire));

    unblock_tx.send(()).unwrap();

    let prep_res = handle_prep.join().unwrap();
    handle_close.join().unwrap();

    assert!(closed_flag.load(Ordering::Acquire));
    assert!(prep_res.is_ok());

    let catalog = runtime.catalog.lock();
    assert!(catalog.entries.is_empty());
    drop(catalog);
    drop(runtime);

    assert!(source_dropped.load(Ordering::Acquire));
}

#[test]
fn reentrant_drop_safety() {
    struct ReentrantSource {
        runtime: Arc<SubscriptionRuntime>,
    }

    impl Drop for ReentrantSource {
        fn drop(&mut self) {
            let _ = self.runtime.cleanup_result();
        }
    }

    impl RtdSource for ReentrantSource {
        type Value = RtdValue;
        type Subscription = TestSubscription;
        fn subscribe(
            &self,
            _: &RtdTopic,
            _: RtdSink<Self::Value>,
        ) -> XllResult<Self::Subscription> {
            Ok(TestSubscription {
                canceled: Arc::new(AtomicBool::new(false)),
                disconnected: Arc::new(AtomicBool::new(false)),
            })
        }
    }

    let fixture = SourceFixture::new();
    let runtime_for_source = Arc::new(SubscriptionRuntime::new());
    let source = fixture
        .registration
        .register(ReentrantSource {
            runtime: Arc::clone(&runtime_for_source),
        })
        .unwrap();
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("reentrant").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    runtime.close().unwrap();
}

#[test]
fn server_lifecycle_rejects_mutations_when_closing() {
    let (arena, source, _, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("test").unwrap())
        .unwrap();
    let id = prep.id();

    server.test_server().publish.mark_closing_for_test();

    assert!(matches!(
        server.attach_update_notifier(RtdNotifier::for_test(Arc::new(TestNotifierState::new()))),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        server.pulse_notification(),
        Err(XllError::Closing)
    ));
    assert!(matches!(server.begin_refresh(), Err(XllError::Closing)));
    assert!(matches!(server.claim(id), Err(XllError::Closing)));
    assert!(matches!(
        server.disconnect(TopicId(1)),
        Err(XllError::Closing)
    ));
}

pub(crate) struct FailingDisconnectSubscription;
// SAFETY: test subscription returns an error on disconnect without unsafe effects.
unsafe impl RtdSubscription for FailingDisconnectSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL,
        })
    }
}

#[test]
fn server_terminate_returns_cleanup_error_to_caller_and_waiter() {
    let (arena, source, _, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("test_err").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    {
        server
            .test_server()
            .subscriptions
            .lock()
            .insert(TopicId(1), Box::new(FailingDisconnectSubscription));
    }

    let server_clone = server;
    let handle = std::thread::spawn(move || server_clone.terminate());

    let res_owner = server.terminate();
    let res_waiter = handle.join().unwrap();

    assert!(matches!(
        res_owner,
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        }) | Err(XllError::Panic)
    ));
    assert!(matches!(
        res_waiter,
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        }) | Err(XllError::Panic)
    ));
}

#[test]
fn server_terminate_callback_drop_failure_reaches_waiter() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let mut state = TestNotifierState::new();
    state.panicking_drop = true;
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::new(state)))
        .unwrap();

    let admission = server.test_server().begin_termination(&runtime);
    let TerminationAdmission::Owner(owner) = admission else {
        panic!("expected Owner admission");
    };

    let handle = std::thread::spawn(move || server.terminate());

    let cancel_res = owner.request_cancel();
    let res_owner = owner.finish(cancel_res);
    assert!(matches!(res_owner, Err(XllError::Panic)));

    let res_waiter = handle.join().unwrap();
    assert!(matches!(res_waiter, Err(XllError::Panic)));
}

#[test]
fn server_terminate_owner_unwind_notifies_waiter() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let admission = server.test_server().begin_termination(&runtime);
    let TerminationAdmission::Owner(owner) = admission else {
        panic!("expected Owner admission");
    };

    let handle = std::thread::spawn(move || server.terminate());

    PANIC_AFTER_TERMINATION_GUARD.set(true);

    let cancel_res = owner.request_cancel();
    let res_owner = catch_unwind(AssertUnwindSafe(|| owner.finish(cancel_res)));
    assert!(res_owner.is_err(), "owner finish should have panicked");

    let res_waiter = handle.join().unwrap();
    assert!(matches!(res_waiter, Err(XllError::Panic)));
}

#[test]
fn disconnect_propagates_subscription_cleanup_error() {
    let (arena, source, _, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("disc_err").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    {
        server
            .test_server()
            .subscriptions
            .lock()
            .insert(TopicId(1), Box::new(FailingDisconnectSubscription));
    }

    let error = server.disconnect(TopicId(1)).unwrap_err();
    assert!(matches!(
        error,
        XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        }
    ));
}

#[test]
fn rollback_records_subscription_cleanup_error() {
    let (arena, source, _, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("roll_err").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let mut conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();

    {
        server
            .test_server()
            .subscriptions
            .lock()
            .insert(TopicId(1), Box::new(FailingDisconnectSubscription));
    }

    conn.rollback();

    assert!(matches!(
        runtime.cleanup_result(),
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        })
    ));
}

struct PanickingCancelSubscription;
// SAFETY: test subscription tests cancel panic handling without unsafe effects.
unsafe impl RtdSubscription for PanickingCancelSubscription {
    fn request_cancel(&self) {
        panic!("request_cancel panic test");
    }
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        Ok(())
    }
}

#[test]
fn request_cancel_panic_propagates_to_termination() {
    let (arena, source, _, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("cancel_panic").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    {
        server
            .test_server()
            .subscriptions
            .lock()
            .insert(TopicId(1), Box::new(PanickingCancelSubscription));
    }

    let res = server.terminate();
    assert!(matches!(res, Err(XllError::Panic)));
}

struct DelayedSubscribeFailingSource {
    tx_entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    rx_close: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl RtdSource for DelayedSubscribeFailingSource {
    type Value = f64;
    type Subscription = FailingDisconnectSubscription;
    fn subscribe(
        &self,
        _topic: &RtdTopic,
        _sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription> {
        if let Some(tx) = self.tx_entered.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let rx = self.rx_close.lock();
        let _ = rx.recv();
        Ok(FailingDisconnectSubscription)
    }
}

#[test]
fn install_failure_during_closing_propagates_cleanup_error() {
    let (tx_close, rx_close) = std::sync::mpsc::channel();
    let (tx_enter, rx_enter) = std::sync::mpsc::channel();

    let (arena, source) = SourceArena::with_source(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        DelayedSubscribeFailingSource {
            tx_entered: std::sync::Mutex::new(Some(tx_enter)),
            rx_close: Mutex::new(rx_close),
        },
    )
    .unwrap();
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prep = runtime
        .prepare(&source, RtdTopic::single("delayed_fail").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let runtime_clone = Arc::clone(&runtime);
    let server_clone = server;
    let id_clone = id;

    let handle = std::thread::spawn(move || {
        runtime_clone.connect_transaction(&server_clone, TopicId(1), id_clone)
    });

    rx_enter.recv().unwrap();

    server.test_server().publish.mark_closing_for_test();

    tx_close.send(()).unwrap();

    let res = handle.join().unwrap();
    assert!(matches!(
        res,
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        })
    ));

    assert!(matches!(
        runtime.cleanup_result(),
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL
        })
    ));
}

#[test]
fn same_handle_and_same_topic_reuse_pending_identity() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let topic = RtdTopic::single("shared").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();
    let second = runtime.prepare(&source, topic).unwrap();

    assert_eq!(first.key(), second.key());
    assert!(first.has_reservation());
    assert!(second.has_reservation());

    second.rollback();
    first.rollback();

    assert!(runtime.catalog.lock().entries.is_empty());
}

#[test]
fn distinct_handles_do_not_share_source_identity() {
    let fixture = SourceFixture::new();
    let (source_a, _, _) = fixture.add::<f64>(None);
    let (source_b, _, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));
    let topic = RtdTopic::single("shared").unwrap();

    let first = runtime.prepare(&source_a, topic.clone()).unwrap();
    let second = runtime.prepare(&source_b, topic).unwrap();

    assert_ne!(first.key(), second.key());

    first.rollback();
    second.rollback();
}

#[test]
fn same_handle_reuses_active_subscription_identity() {
    let (arena, source, _, _) = publishing_source(Some(1.0_f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let topic = RtdTopic::single("shared-active").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();
    let id = first.id();
    let key = *first.key();
    first.commit();

    let connection = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    connection.commit().unwrap();

    let second = runtime.prepare(&source, topic).unwrap();

    assert_eq!(second.id(), id);
    assert_eq!(second.key(), &key);
    assert!(!second.has_reservation());

    // ExistingActiveに対するrollbackは既存subscriptionを壊さない。
    second.rollback();

    assert!(
        runtime
            .catalog
            .lock()
            .entries
            .get(&id)
            .is_some_and(|entry| entry.is_active())
    );
}

#[test]
fn released_source_identity_returns_to_the_live_quota() {
    let fixture = SourceFixture::new();
    let (first_source, _, _) = fixture.add::<f64>(None);
    let (second_source, _, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_host(
        RuntimeGeneration::new(1).expect("test generation is non-zero"),
        RtdLimits {
            max_source_ids: RtdCapacity::from_usize(1),
            ..RtdLimits::standard()
        },
        RtdSubscriptionHost::default(),
        fixture.finish(),
    ));

    runtime
        .prepare(&first_source, RtdTopic::single("first").unwrap())
        .unwrap()
        .rollback();
    let _ = first_source;

    runtime
        .prepare(&second_source, RtdTopic::single("second").unwrap())
        .expect("a released source identity returns to the live quota")
        .rollback();
}

#[test]
fn live_source_reuses_identity_after_pending_subscription_is_removed() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let topic = RtdTopic::single("stable").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();

    let first_key = *first.key();
    first.rollback();

    let second = runtime.prepare(&source, topic).unwrap();

    assert_ne!(second.key(), &first_key);
}

#[test]
fn failed_pending_admission_rolls_back_new_source_identity() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_host(
        RuntimeGeneration::new(1).expect("test generation is non-zero"),
        RtdLimits {
            max_pending: RtdCapacity::disabled(),
            max_source_ids: RtdCapacity::from_usize(1),
            ..RtdLimits::standard()
        },
        RtdSubscriptionHost::default(),
        arena,
    ));

    assert!(matches!(
        runtime.prepare(&source, RtdTopic::single("blocked").unwrap()),
        Err(XllError::Overloaded)
    ));

    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 0);
}

#[test]
fn source_refcount_tracks_live_subscription_identities() {
    let mut index = SubscriptionIdentityIndex::default();
    let (_arena, source, _, _) = publishing_source::<f64>(None);
    let first_identity = SubscriptionIdentity {
        source_id: SourceId(source.id),
        topic: RtdTopic::single("first").unwrap(),
    };
    let second_identity = SubscriptionIdentity {
        source_id: SourceId(source.id),
        topic: RtdTopic::single("second").unwrap(),
    };
    let first_id = SubscriptionId(1);
    let second_id = SubscriptionId(2);

    index.insert(first_identity.clone(), first_id, 16).unwrap();
    index
        .insert(second_identity.clone(), second_id, 16)
        .unwrap();
    assert_eq!(
        index.source_ref_count(source.id).map(|refs| refs.get()),
        Some(2)
    );
    assert_eq!(index.distinct_source_count(), 1);

    index.remove(&first_identity);
    assert_eq!(
        index.source_ref_count(source.id).map(|refs| refs.get()),
        Some(1)
    );
    index.remove(&second_identity);
    assert_eq!(index.distinct_source_count(), 0);
    index.assert_invariants();
}

#[test]
fn source_limit_counts_distinct_live_sources_not_topics() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_host(
        RuntimeGeneration::new(1).expect("test generation is non-zero"),
        RtdLimits {
            max_source_ids: RtdCapacity::from_usize(1),
            ..RtdLimits::standard()
        },
        RtdSubscriptionHost::default(),
        arena,
    ));

    let first = runtime
        .prepare(&source, RtdTopic::single("first-topic").unwrap())
        .unwrap();
    let second = runtime
        .prepare(&source, RtdTopic::single("second-topic").unwrap())
        .unwrap();

    let catalog = runtime.catalog.lock();
    assert_eq!(catalog.identities.distinct_source_count(), 1);
    assert_eq!(
        catalog
            .identities
            .source_ref_count(source.id)
            .map(|refs| refs.get()),
        Some(2)
    );
    drop(catalog);

    first.rollback();
    second.rollback();
    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 0);
}

#[test]
fn source_limit_rejects_a_second_live_source() {
    let fixture = SourceFixture::new();
    let (source_a, _, _) = fixture.add::<f64>(None);
    let (source_b, _, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_host(
        RuntimeGeneration::new(1).expect("test generation is non-zero"),
        RtdLimits {
            max_source_ids: RtdCapacity::from_usize(1),
            ..RtdLimits::standard()
        },
        RtdSubscriptionHost::default(),
        fixture.finish(),
    ));

    let first = runtime
        .prepare(&source_a, RtdTopic::single("first-source").unwrap())
        .unwrap();
    assert!(matches!(
        runtime.prepare(&source_b, RtdTopic::single("second-source").unwrap()),
        Err(XllError::Overloaded)
    ));

    let catalog = runtime.catalog.lock();
    assert_eq!(catalog.identities.distinct_source_count(), 1);
    assert_eq!(
        catalog
            .identities
            .source_ref_count(source_a.id)
            .map(|n| n.get()),
        Some(1)
    );
    assert_eq!(catalog.identities.source_ref_count(source_b.id), None);
    drop(catalog);

    first.rollback();
    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 0);
}

#[test]
fn duplicate_identity_does_not_change_source_refcount() {
    let mut index = SubscriptionIdentityIndex::default();
    let (_arena, source, _, _) = publishing_source::<f64>(None);
    let identity = SubscriptionIdentity {
        source_id: SourceId(source.id),
        topic: RtdTopic::single("duplicate").unwrap(),
    };
    let first_id = SubscriptionId(1);
    let second_id = SubscriptionId(2);

    index.insert(identity.clone(), first_id, 1).unwrap();
    assert!(matches!(
        index.insert(identity, second_id, 1),
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_DUPLICATE
        })
    ));
    assert_eq!(index.source_ref_count(source.id).map(|n| n.get()), Some(1));
    assert_eq!(index.id_by_identity.len(), 1);
    index.assert_invariants();
}

#[test]
fn topic_part_boundaries_are_part_of_subscription_identity() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));

    let topic_a = RtdTopic::new(["a\0b", "c"]).unwrap();
    let topic_b = RtdTopic::new(["a", "b\0c"]).unwrap();

    let prepared_a = runtime.prepare(&source, topic_a).unwrap();

    let prepared_b = runtime.prepare(&source, topic_b).unwrap();

    assert_ne!(prepared_a.key(), prepared_b.key());
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn structurally_equal_topics_share_transport_key() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));

    let prepared_a = runtime
        .prepare(&source, RtdTopic::new(["market", "USD\0JPY"]).unwrap())
        .unwrap();

    let prepared_b = runtime
        .prepare(&source, RtdTopic::new(["market", "USD\0JPY"]).unwrap())
        .unwrap();

    assert_eq!(prepared_a.key(), prepared_b.key());
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn topic_rejects_parts_larger_than_excel_string_limit() {
    let part = "x".repeat(crate::utf16::EXCEL_STRING_LIMIT + 1);
    assert!(RtdTopic::single(part).is_err());
}

#[test]
fn large_logical_topic_uses_bounded_transport_key() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));

    let topic = RtdTopic::single("x".repeat(16 * 1024)).unwrap();
    let prepared = runtime.prepare(&source, topic).unwrap();

    let transport = prepared.key().to_transport();
    assert_eq!(transport.encode_utf16().count(), 43);
    assert!(transport.starts_with("stream:v1:"));
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn distinct_identities_receive_distinct_transport_keys() {
    let fixture = SourceFixture::new();
    let (source_a, _, _) = fixture.add::<f64>(None);
    let (source_b, _, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let topic = RtdTopic::single("same").unwrap();

    let a = runtime.prepare(&source_a, topic.clone()).unwrap();
    let b = runtime.prepare(&source_b, topic).unwrap();

    assert_ne!(a.key(), b.key());
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn identity_index_is_removed_after_final_unbind() {
    let (arena, source, _, _) = publishing_source(Some(1.0_f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prepared = runtime
        .prepare(&source, RtdTopic::single("unbind_test").unwrap())
        .unwrap();
    let id = prepared.id();
    prepared.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    runtime.disconnect(&server, TopicId(1)).unwrap();

    let catalog = runtime.catalog.lock();
    assert!(catalog.entries.is_empty());
    assert!(catalog.identities.id_by_identity.is_empty());
    catalog.assert_identity_invariants();
}

#[test]
fn catalog_entries_are_canonical_for_subscription_identity() {
    let fixture = SourceFixture::new();
    let (source_a, _, _) = fixture.add::<f64>(None);
    let (source_b, _, _) = fixture.add::<f64>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(
        fixture.finish(),
    ));

    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("topic-a").unwrap())
        .unwrap();
    let prep_b = runtime
        .prepare(&source_b, RtdTopic::single("topic-b").unwrap())
        .unwrap();

    let catalog = runtime.catalog.lock();
    catalog.assert_identity_invariants();
    assert_eq!(catalog.identities.id_by_identity.len(), 2);
    assert_eq!(catalog.entries.len(), 2);

    for (identity, id) in &catalog.identities.id_by_identity {
        let entry = catalog.entries.get(id).unwrap();
        assert_eq!(entry.source_id, identity.source_id);
        assert_eq!(&entry.topic, &identity.topic);
    }
    drop(catalog);

    prep_a.rollback();
    prep_b.rollback();

    let catalog = runtime.catalog.lock();
    assert!(catalog.entries.is_empty());
    assert!(catalog.identities.id_by_identity.is_empty());
    catalog.assert_identity_invariants();
}

#[test]
fn transport_key_parser_rejects_noncanonical_keys() {
    for invalid in [
        "stream:",
        "stream:v2:0000000000000001:0000000000000001",
        "stream:v1:1:2",
        "stream:v1:000000000000000g:0000000000000001",
        "stream:v1:0000000000000001:0000000000000001:extra",
    ] {
        assert!(SubscriptionKey::parse_transport(invalid).is_err());
    }
}

#[test]
fn subscription_key_round_trips_through_transport() {
    let key = SubscriptionKey::from_allocated_id(1, 42);
    let transport = key.to_transport();

    assert_eq!(transport, "stream:v1:0000000000000001:000000000000002a");
    assert_eq!(SubscriptionKey::parse_transport(&transport).unwrap(), key);
}

#[test]
fn refresh_state_attach_prepare_commit_lifecycle() {
    let mut state: RefreshState<u32> = RefreshState::default();
    assert_eq!(state.attach_notifier(100), None);

    let prepared = state.prepare_notification(true).unwrap().unwrap();
    assert_eq!(prepared.ticket, 0);
    assert_eq!(prepared.notifier, 100);

    let attempt = state.commit_notification(prepared);
    assert_eq!(attempt.ticket, 0);
    assert_eq!(attempt.notifier, 100);

    // While Calling, prepare_notification returns None
    assert!(state.prepare_notification(true).unwrap().is_none());

    // Calling signal can be inspected
    assert!(state.signal_calling_mut(0).is_some());
    assert!(state.signal_calling_mut(1).is_none());

    // Detach notifier resets signal to Dormant
    assert_eq!(state.detach_notifier(), Some(100));
}

#[test]
fn server_notification_retry_sequence_eventually_succeeds() {
    let (arena, source, sink, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));
    state.outcomes.lock().push_back(TestNotifyOutcome::Success);

    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("retry_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    let sink = sink.lock().clone().unwrap();
    sink.publish(1.0).unwrap();

    // 2 errors followed by 1 success -> 3 calls total
    assert_eq!(state.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn server_notification_retry_suppressed_after_max_attempts() {
    let (arena, source, sink, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    // 4 consecutive errors
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));
    state
        .outcomes
        .lock()
        .push_back(TestNotifyOutcome::Error(XllError::Panic));

    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("suppress_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    let sink = sink.lock().clone().unwrap();
    sink.publish(1.0).unwrap();

    // Max 3 attempts allowed, then suppressed
    assert_eq!(state.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn server_notification_panic_records_cleanup_failure() {
    let (arena, source, sink, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    state.outcomes.lock().push_back(TestNotifyOutcome::Panic);

    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("panic_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    let sink = sink.lock().clone().unwrap();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        sink.publish(1.0).unwrap();
    }));
    assert!(res.is_err());
    assert!(runtime.cleanup_result().is_err());
}

#[test]
fn runtime_close_causes_fail_closed_on_server() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    runtime.close().unwrap();

    assert!(matches!(
        server.test_server().enter_operation(),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        server.test_server().enter_owned_operation(),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        server.test_server().publish.publish(
            TopicId(1),
            ConnectionGeneration::new(1).unwrap(),
            RtdValue::Number(1.0).into_stored().unwrap(),
        ),
        Err(XllError::Closing)
    ));
}

#[test]
fn runtime_close_and_publish_race() {
    let (arena, source, sink, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prep = runtime
        .prepare(&source, RtdTopic::single("race_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();
    let sink = sink.lock().clone().unwrap();

    let sink_clone = sink.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (start_tx, start_rx) = std::sync::mpsc::channel::<()>();
    let start_rx = Arc::new(Mutex::new(start_rx));

    let mut handles = Vec::new();
    for i in 0..8 {
        let sink = sink_clone.clone();
        let ready_tx = ready_tx.clone();
        let start_rx = Arc::clone(&start_rx);
        handles.push(std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _ = start_rx.lock().recv();
            for j in 0..100 {
                let _ = sink.publish((i * 100 + j) as f64);
            }
        }));
    }
    drop(ready_tx);
    for _ in 0..8 {
        ready_rx.recv().unwrap();
    }

    let runtime_close = Arc::clone(&runtime);
    let close_handle = std::thread::spawn(move || {
        runtime_close.close().unwrap();
    });

    drop(start_tx);

    for h in handles {
        h.join().unwrap();
    }
    close_handle.join().unwrap();

    assert!(matches!(sink.publish(999.0), Err(XllError::Closing)));
}

#[test]
fn quota_permit_releases_on_drain() {
    let (arena, source, sink_slot, _) = publishing_source(Some(0.0f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let prep = runtime
        .prepare(&source, RtdTopic::single("quota_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();
    let sink = sink_slot.lock().clone().unwrap();

    sink.publish(42.0).unwrap();
    assert_eq!(runtime.queued_update_quota.used(), 1);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    batch
        .complete(crate::subscription::RefreshOutcome::Delivered)
        .unwrap();
    assert_eq!(runtime.queued_update_quota.used(), 0);
}

pub(crate) struct SinkHoldingSubscription<T> {
    _sink: RtdSink<T>,
}

// SAFETY: test subscription holds an RtdSink and does not touch unsafe state.
unsafe impl<T: Send + 'static> RtdSubscription for SinkHoldingSubscription<T> {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        Ok(())
    }
}

struct SinkCapturingSource;

impl RtdSource for SinkCapturingSource {
    type Value = f64;
    type Subscription = SinkHoldingSubscription<Self::Value>;
    fn subscribe(
        &self,
        _topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Self::Subscription> {
        Ok(SinkHoldingSubscription { _sink: sink })
    }
}

#[test]
fn publish_core_drops_cleanly_without_cycle_when_subscription_holds_sink() {
    let (arena, source) = SourceArena::with_source(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        SinkCapturingSource,
    )
    .unwrap();
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let prep = runtime
        .prepare(&source, RtdTopic::single("cycle_test").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    // Terminate server, closing and disconnecting subscriptions
    server.terminate().unwrap();

    // After termination, server is marked closed/terminated
    assert!(server.claim(id).is_err());
}

#[test]
fn prepare_warm_path_reuses_registered_source_identity() {
    let (arena, source, _, _) = publishing_source(Some(1.0_f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let topic = RtdTopic::single("warm-path-strong-count").unwrap();

    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 0);

    // 1. Initial prepare registers the handle identity and creates the pending subscription.
    let first = runtime.prepare(&source, topic.clone()).unwrap();
    assert!(first.has_reservation());
    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 1);

    // 2. ExistingPending prepare reuses the same handle identity and pending entry.
    let second_pending = runtime.prepare(&source, topic.clone()).unwrap();
    assert!(second_pending.has_reservation());
    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 1);
    second_pending.rollback();

    // Commit and connect transaction to activate subscription.
    // The pending catalog entry is consumed/removed.
    let id = first.id();
    first.commit();
    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    // 3. ExistingActive prepare is a warm lookup without a new source identity.
    let warm_prepared = runtime.prepare(&source, topic).unwrap();
    assert!(!warm_prepared.has_reservation());
    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 1);
    warm_prepared.rollback();
}

#[test]
fn existing_active_does_not_downgrade_runtime_or_mutate_catalog() {
    let (arena, source, _, _) = publishing_source(Some(1.0_f64));
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let topic = RtdTopic::single("existing-active-noop").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();
    let id = first.id();
    let key = *first.key();
    first.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    // Prepare on existing active: warm lookup
    let warm = runtime.prepare(&source, topic).unwrap();
    assert_eq!(warm.id(), id);
    assert_eq!(warm.key(), &key);
    assert!(!warm.has_reservation());

    // Rollback is a no-op: catalog active keys and pending are untouched
    warm.rollback();
    let catalog = runtime.catalog.lock();
    assert!(
        catalog
            .entries
            .get(&id)
            .is_some_and(|entry| entry.is_active())
    );
    assert_eq!(catalog.pending_len(), 0);
}

#[test]
fn resolve_transport_key_validates_runtime_identity() {
    let (arena, source, _, _) = publishing_source::<f64>(None);
    let runtime_a = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let runtime_b = Arc::new(SubscriptionRuntime::new());

    let prep = runtime_a
        .prepare(&source, RtdTopic::single("resolve-test").unwrap())
        .unwrap();

    let id = prep.id();
    let key = *prep.key();

    // 1. Valid current-runtime key resolves to SubscriptionId
    assert_eq!(runtime_a.resolve_transport_key(key).unwrap(), id);

    // 2. Key from another runtime fails with StaleHandle
    assert!(matches!(
        runtime_b.resolve_transport_key(key),
        Err(XllError::StaleHandle)
    ));

    // 3. Round-trip transport string
    let transport = key.to_transport();
    let parsed_key = SubscriptionKey::parse_transport(&transport).unwrap();
    assert_eq!(runtime_a.resolve_transport_key(parsed_key).unwrap(), id);
}

#[test]
fn double_slot_pending_metadata_and_values_invariants() {
    // Invariants 2, 3, 4:
    // - For any pending update in pending[b], active.values[b] exists with matching (generation, sequence)
    // - latest_slot is Some and points to a Some value slot
    let (_runtime, server, sink) = connected_sink::<String>(None, "double-slot-invariants");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);
    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        let latest_slot = active.latest_slot.expect("latest_slot must be Some") as usize;
        assert!(matches!(active.values[latest_slot], ValueSlot::Resident(_)));

        for b in [0, 1] {
            if let Some(queued) = shard.pending[b].get(&topic_id) {
                let ValueSlot::Resident(slot) = &active.values[b] else {
                    panic!("values[b] must be Resident matching pending");
                };
                assert_eq!(slot.generation, queued.connection_generation);
                assert_eq!(slot.sequence, queued.sequence);
            }
        }
    }
}

#[test]
fn refresh_failure_preserves_pending_and_values_for_retry() {
    // Invariant 5: Refresh failure does not alter pending/value state.
    let (_runtime, server, sink) = connected_sink::<String>(None, "refresh-failure-retry");

    sink.publish("retry-value".to_owned()).unwrap();
    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(
        batch.updates[0].value,
        StoredRtdValue::String("retry-value".into())
    );
    batch.complete(RefreshOutcome::Failed).unwrap();

    // After failure, retry succeeds with identical value and sequence!
    let retry_batch = server.begin_refresh().unwrap();
    assert_eq!(retry_batch.updates.len(), 1);
    assert_eq!(
        retry_batch.updates[0].value,
        StoredRtdValue::String("retry-value".into())
    );
    retry_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn newer_publish_after_refresh_snapshot_reads_old_slot_value() {
    // Invariant 6: Newer publish arriving after snapshot boundary writes to the other slot;
    // the old snapshot still holds the old slot's value, and subsequent refresh reads the newer value.
    let (_runtime, server, sink) = connected_sink::<String>(None, "snapshot-boundary-isolation");

    sink.publish("v1-slot0".to_owned()).unwrap();
    let snapshot = server.begin_refresh().unwrap();
    assert_eq!(snapshot.updates.len(), 1);
    assert_eq!(
        snapshot.updates[0].value,
        StoredRtdValue::String("v1-slot0".into())
    );

    // While older refresh is in-flight before completion, publish a newer value into the other slot
    sink.publish("v2-slot1".to_owned()).unwrap();

    // The in-flight snapshot completes delivery of v1-slot0
    snapshot.complete(RefreshOutcome::Delivered).unwrap();

    // Next refresh collects v2-slot1
    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(next_batch.updates.len(), 1);
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v2-slot1".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn successful_refresh_retains_latest_value_for_exact_dedup() {
    // Invariant 7: Successful refresh retains the latest value in values[latest_slot] for exact dedup.
    let (_runtime, server, sink) = connected_sink::<String>(None, "retain-latest-dedup");

    sink.publish("dedup-target".to_owned()).unwrap();
    let batch = server.begin_refresh().unwrap();
    batch.complete(RefreshOutcome::Delivered).unwrap();

    // Publishing identical value must be suppressed (zero queued updates)
    sink.publish("dedup-target".to_owned()).unwrap();
    assert_eq!(server.pending_update_count(), 0);

    // Publishing changed value produces an update
    sink.publish("changed-target".to_owned()).unwrap();
    assert_eq!(server.pending_update_count(), 1);
    let batch2 = server.begin_refresh().unwrap();
    assert_eq!(
        batch2.updates[0].value,
        StoredRtdValue::String("changed-target".into())
    );
    batch2.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn non_latest_delivered_slot_is_reclaimed() {
    // Invariant 8: Delivered slot that is not the latest_slot is reclaimed to None.
    let (_runtime, server, sink) = connected_sink::<String>(None, "non-latest-reclamation");

    sink.publish("v1".to_owned()).unwrap();
    let planned = server.test_server().publish.plan_refresh().unwrap();
    sink.publish("v2".to_owned()).unwrap();

    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);
    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::Resident(_)));
        assert!(matches!(active.values[1], ValueSlot::Resident(_)));
        assert_eq!(active.latest_slot, Some(1));
    }

    let batch = planned.collect();
    batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(
            matches!(active.values[0], ValueSlot::Empty),
            "non-latest delivered slot must be reclaimed to Empty"
        );
        assert!(
            matches!(active.values[1], ValueSlot::Resident(_)),
            "latest slot must be retained for dedup"
        );
        assert_eq!(active.latest_slot, Some(1));
    }
}

#[test]
fn inflight_refresh_batch_leases_value_without_double_ownership() {
    // Invariants 1 & 2:
    // - InFlight slot corresponds to exactly one in-flight lease entry.
    // - Resident and lease never own the same value simultaneously.
    let (_runtime, server, sink) = connected_sink::<String>(None, "phase-b-inv-1-2");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::Resident(_)));
    }

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);
    assert_eq!(batch.updates[0].value, StoredRtdValue::String("v1".into()));

    // While batch is held, the slot in the shard MUST be InFlight, NOT Resident!
    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert_eq!(
            active.values[0],
            ValueSlot::InFlight {
                generation: batch.updates[0].connection_generation,
                sequence: batch.updates[0].sequence,
            }
        );
        assert!(!matches!(active.values[0], ValueSlot::Resident(_)));
    }

    batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn newer_publish_during_refresh_preserves_inflight_lease() {
    // Invariant 3: Newer publish during refresh writes into the other writer slot
    // without blocking or corrupting in-flight lease.
    let (_runtime, server, sink) = connected_sink::<String>(None, "phase-b-inv-3");

    sink.publish("v1-slot0".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(
        batch.updates[0].value,
        StoredRtdValue::String("v1-slot0".into())
    );

    // While batch is in flight (slot 0 InFlight), publish to topic
    sink.publish("v2-slot1".to_owned()).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Slot 0 is still InFlight for batch
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        // Slot 1 has the new value as Resident
        assert!(matches!(active.values[1], ValueSlot::Resident(_)));
        assert_eq!(active.latest_slot, Some(1));
    }

    // Complete batch
    batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Old slot 0 was reclaimed to Empty because latest_slot is 1
        assert_eq!(active.values[0], ValueSlot::Empty);
        // Slot 1 still holds v2-slot1
        assert!(matches!(active.values[1], ValueSlot::Resident(_)));
        assert_eq!(active.latest_slot, Some(1));
    }

    // Next refresh collects slot 1
    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(next_batch.updates.len(), 1);
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v2-slot1".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn publish_during_inflight_refresh_treats_same_value_conservatively() {
    // Invariant 4: While InFlight is latest, same-value publish is treated conservatively
    // as changed (does not dedup) and publishes to the other slot.
    let (_runtime, server, sink) = connected_sink::<String>(None, "phase-b-inv-4");

    sink.publish("same-value".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(
        batch.updates[0].value,
        StoredRtdValue::String("same-value".into())
    );

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        assert_eq!(active.latest_slot, Some(0));
    }

    // While slot 0 is InFlight, publishing the EXACT same value does NOT dedup:
    // it conservatively publishes to slot 1!
    sink.publish("same-value".to_owned()).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        assert!(matches!(active.values[1], ValueSlot::Resident(_)));
        assert_eq!(active.latest_slot, Some(1));
    }

    // Complete first batch
    batch.complete(RefreshOutcome::Delivered).unwrap();

    // Next batch collects the conservatively published update
    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(next_batch.updates.len(), 1);
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("same-value".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();

    // Now slot 1 is Resident and latest. Publishing "same-value" again DOES exact-dedup!
    sink.publish("same-value".to_owned()).unwrap();
    assert_eq!(server.pending_update_count(), 0);
}

#[test]
fn delivered_refresh_restores_resident_value_for_subsequent_dedup() {
    // Invariant 5: Successful completion restores value to Resident if still latest.
    let (_runtime, server, sink) = connected_sink::<String>(None, "phase-b-inv-5");

    sink.publish("target-value".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(
        batch.updates[0].value,
        StoredRtdValue::String("target-value".into())
    );

    // No intermediate publish occurs
    batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(
            &active.values[0],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("target-value".into())
        ));
        assert_eq!(active.latest_slot, Some(0));
    }

    // Since it was restored to Resident, subsequent publish of same value dedups!
    sink.publish("target-value".to_owned()).unwrap();
    assert_eq!(server.pending_update_count(), 0);
}

#[test]
fn failed_refresh_and_batch_drop_restore_resident_value_for_retry() {
    // Invariant 6: Failed completion or batch drop restores value to Resident
    // if slot is InFlight (enables lossless retry).
    let (_runtime, server, sink) = connected_sink::<String>(None, "phase-b-inv-6");

    // Test case 6a: Explicit failure
    sink.publish("retry-val-1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    batch.complete(RefreshOutcome::Failed).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(
            &active.values[0],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("retry-val-1".into())
        ));
    }

    // Retry succeeds
    let retry1 = server.begin_refresh().unwrap();
    assert_eq!(
        retry1.updates[0].value,
        StoredRtdValue::String("retry-val-1".into())
    );
    retry1.complete(RefreshOutcome::Delivered).unwrap();

    // Test case 6b: Batch drop without complete (abort)
    sink.publish("retry-val-2".to_owned()).unwrap();
    let batch_drop = server.begin_refresh().unwrap();
    assert_eq!(
        batch_drop.updates[0].value,
        StoredRtdValue::String("retry-val-2".into())
    );
    drop(batch_drop); // aborted!

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        let slot = active.latest_slot.unwrap() as usize;
        assert!(matches!(
            &active.values[slot],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("retry-val-2".into())
        ));
    }

    let retry2 = server.begin_refresh().unwrap();
    assert_eq!(
        retry2.updates[0].value,
        StoredRtdValue::String("retry-val-2".into())
    );
    retry2.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn newer_different_publish_supersedes_inflight_slot() {
    // InFlight(latest) + different publish
    let (_runtime, server, sink) = connected_sink::<String>(None, "sm-inflight-different");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates[0].value, StoredRtdValue::String("v1".into()));

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        assert_eq!(active.latest_slot, Some(0));
    }

    // Publish different value while InFlight is latest
    sink.publish("v2".to_owned()).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v2".into())
        ));
        assert_eq!(active.latest_slot, Some(1));
    }

    // Complete the first batch
    batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Slot 0 is reclaimed to Empty because latest_slot is 1
        assert_eq!(active.values[0], ValueSlot::Empty);
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v2".into())
        ));
    }

    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v2".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn older_publish_is_ignored_when_slot_is_inflight() {
    // InFlight(old) + publish:
    // Slot 0 in-flight, slot 1 published, and then another publish arrives to slot 1 before delivery
    let (_runtime, server, sink) = connected_sink::<String>(None, "sm-inflight-old-publish");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates[0].value, StoredRtdValue::String("v1".into()));

    // Newer publish writes to slot 1
    sink.publish("v2".to_owned()).unwrap();
    // Another publish writes to slot 1 again
    sink.publish("v3".to_owned()).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Slot 0 must remain InFlight untouched
        assert!(matches!(active.values[0], ValueSlot::InFlight { .. }));
        // Slot 1 must hold v3
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v3".into())
        ));
        assert_eq!(active.latest_slot, Some(1));
    }

    batch.complete(RefreshOutcome::Delivered).unwrap();

    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v3".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn failed_refresh_does_not_overwrite_newer_published_value() {
    // Failed after newer publish:
    // Slot 0 in-flight, slot 1 published with newer value, then batch fails.
    // Slot 0 is restored to Resident, but latest_slot remains 1. Next refresh delivers slot 1.
    let (_runtime, server, sink) = connected_sink::<String>(None, "sm-failed-after-newer");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates[0].value, StoredRtdValue::String("v1".into()));

    sink.publish("v2".to_owned()).unwrap();

    // First batch fails!
    batch.complete(RefreshOutcome::Failed).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Slot 0 is restored to Resident
        assert!(matches!(
            &active.values[0],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v1".into())
        ));
        // Slot 1 is Resident with v2
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v2".into())
        ));
        // latest_slot is 1
        assert_eq!(active.latest_slot, Some(1));
        // Both updates are pending
        assert!(shard.pending[0].contains_key(&topic_id));
        assert!(shard.pending[1].contains_key(&topic_id));
    }

    // Next refresh picks newer sequence (slot 1, v2)!
    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(next_batch.updates.len(), 1);
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v2".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Delivered sequence 1 retires both pending updates and reclaims slot 0 to Empty
        assert_eq!(active.values[0], ValueSlot::Empty);
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v2".into())
        ));
        assert_eq!(active.latest_slot, Some(1));
        assert!(!shard.pending[0].contains_key(&topic_id));
        assert!(!shard.pending[1].contains_key(&topic_id));
    }
}

#[test]
fn dropped_refresh_batch_preserves_newer_published_value() {
    // lease Drop after newer publish:
    // Slot 0 in-flight, slot 1 published with newer value, batch is dropped (aborted).
    let (_runtime, server, sink) = connected_sink::<String>(None, "sm-drop-after-newer");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates[0].value, StoredRtdValue::String("v1".into()));

    sink.publish("v2".to_owned()).unwrap();

    // Batch dropped without complete!
    drop(batch);

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(
            &active.values[0],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v1".into())
        ));
        assert!(matches!(
            &active.values[1],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("v2".into())
        ));
        assert_eq!(active.latest_slot, Some(1));
    }

    // Next refresh picks newer update v2!
    let next_batch = server.begin_refresh().unwrap();
    assert_eq!(
        next_batch.updates[0].value,
        StoredRtdValue::String("v2".into())
    );
    next_batch.complete(RefreshOutcome::Delivered).unwrap();
}

#[test]
fn refresh_batch_completion_is_idempotent() {
    // Verify that a refresh batch cannot be finalized twice, and dropping after completion
    // does not trigger rollback.
    let (_runtime, server, sink) = connected_sink::<String>(None, "sm-no-double-finalization");

    sink.publish("v1".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 1);

    // Explicit complete consumes batch
    batch.complete(RefreshOutcome::Delivered).unwrap();

    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        // Slot 0 is Resident and latest
        assert!(matches!(&active.values[0], ValueSlot::Resident(_)));
        assert_eq!(active.latest_slot, Some(0));
    }

    // Attempting another publish of identical value is properly suppressed (exact dedup)
    sink.publish("v1".to_owned()).unwrap();
    assert_eq!(server.pending_update_count(), 0);
}

#[test]
fn runtime_close_reclaims_all_inflight_slots() {
    // Invariant: InFlight never exists without an active lease batch owning it.
    // Across 10 topics: publish -> refresh -> disconnect/rollback/deliver/fail -> zero InFlight.
    let (arena, source, sink_slot, _) = publishing_source::<String>(None);
    let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let mut sinks = Vec::new();
    for i in 1..=5 {
        let topic_name = format!("orphan-topic-{i}");
        let prepared = runtime
            .prepare(&source, RtdTopic::single(&topic_name).unwrap())
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, TopicId(i), id)
            .unwrap()
            .commit()
            .unwrap();
        let sink = sink_slot.lock().clone().expect("source must capture sink");
        sinks.push(sink);
    }

    for sink in &sinks {
        sink.publish("batch-val".to_owned()).unwrap();
    }

    let batch = server.begin_refresh().unwrap();
    assert_eq!(batch.updates.len(), 5);

    // During active lease: all 5 topics have an InFlight slot
    let check_inflight_count = || -> usize {
        let mut inflight = 0;
        for i in 0..TOPIC_SHARDS {
            let shard = server.test_server().publish.lock_shard_for_test(i);
            for active in shard.active_by_topic.values() {
                for slot in &active.values {
                    if matches!(slot, ValueSlot::InFlight { .. }) {
                        inflight += 1;
                    }
                }
            }
        }
        inflight
    };

    assert_eq!(check_inflight_count(), 5);

    // Complete batch
    batch.complete(RefreshOutcome::Delivered).unwrap();

    // After completion: exactly ZERO slots across all shards may be InFlight!
    assert_eq!(
        check_inflight_count(),
        0,
        "no orphaned InFlight slots allowed after delivery"
    );

    // Second cycle with abort
    for sink in &sinks {
        sink.publish("batch-val-2".to_owned()).unwrap();
    }
    let batch2 = server.begin_refresh().unwrap();
    assert_eq!(check_inflight_count(), 5);
    drop(batch2); // abort!

    // After abort: exactly ZERO slots may be InFlight!
    assert_eq!(
        check_inflight_count(),
        0,
        "no orphaned InFlight slots allowed after abort"
    );
}

#[test]
fn refresh_batch_drop_on_unwind_does_not_deadlock() {
    // AUDIT: Verify that dropping RtdRefreshBatch during panic unwind cleans up
    // without deadlock and restores Resident state.
    let (_runtime, server, sink) = connected_sink::<String>(None, "drop-panic-safety");

    sink.publish("unwind-val".to_owned()).unwrap();
    let topic_id = TopicId(1);
    let shard_idx = shard_index(topic_id);

    let res = catch_unwind(AssertUnwindSafe(|| {
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        panic!("simulated failure during COM formatting / caller processing");
    }));
    assert!(res.is_err(), "closure panicked as expected");

    // After panic unwind: batch was dropped safely without deadlock, and slot was restored!
    {
        let shard = server.test_server().publish.lock_shard_for_test(shard_idx);
        let active = shard.active_by_topic.get(&topic_id).unwrap();
        assert!(matches!(
            &active.values[0],
            ValueSlot::Resident(v) if v.value == StoredRtdValue::String("unwind-val".into())
        ));
    }

    // Subsequent refresh retry succeeds
    let retry = server.begin_refresh().unwrap();
    assert_eq!(
        retry.updates[0].value,
        StoredRtdValue::String("unwind-val".into())
    );
    retry.complete(RefreshOutcome::Delivered).unwrap();
}
