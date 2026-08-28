use super::*;
use crate::excel_rtd::RtdNotifier;
use crate::rtd::test_support::{TestNotifierState, TestNotifyOutcome};

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct TestSubscription {
    canceled: Arc<AtomicBool>,
    disconnected: Arc<AtomicBool>,
}

impl RtdSubscription for TestSubscription {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        let canceled = Arc::clone(&self.canceled);
        Arc::new(RtdCancellationHandle::new(move || {
            canceled.store(true, Ordering::Release);
        }))
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

pub(crate) type PublishingSourceResult<T> = (
    RtdSourceHandle<PublishingSource<T, fn() -> XllResult<()>>>,
    Arc<Mutex<Option<RtdSink<T>>>>,
    Arc<AtomicBool>,
);

pub(crate) fn publishing_source<T: IntoRtdValue + Clone + Send + Sync + 'static>(
    initial: Option<T>,
) -> PublishingSourceResult<T> {
    let slot = Arc::new(Mutex::new(None));
    let disconnected = Arc::new(AtomicBool::new(false));
    let source = RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        PublishingSource {
            initial,
            sink_slot: Arc::clone(&slot),
            canceled: Arc::new(AtomicBool::new(false)),
            disconnected: Arc::clone(&disconnected),
            on_subscribe: None,
        },
    )
    .expect("test source handle allocation must succeed");
    (source, slot, disconnected)
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
    let (source_b, sink_b, _) = publishing_source(Some(2.0f64));

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

    let _sink_a = sink_a.lock().clone().unwrap();
    let sink_b = sink_b.lock().clone().unwrap();

    let lock_guard = server_a.inner.publish.lock_shard_for_test(0);

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
    let runtime = Arc::new(SubscriptionRuntime::new());
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

    let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
    let (source_b, sink_b, _) = publishing_source(Some(2.0f64));

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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
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
    let _guard_a = server_a.inner.publish.lock_shard_for_test(0);

    let (tx, rx) = std::sync::mpsc::channel();
    let server_b_clone = server_b.clone();
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
fn refresh_batch_does_not_retain_server_arc() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let before = Arc::strong_count(&server.inner);

    let batch = server.begin_refresh().unwrap();

    assert_eq!(
        Arc::strong_count(&server.inner),
        before,
        "refresh batch must borrow SubscriptionServer rather than retain an Arc",
    );

    batch.complete(RefreshOutcome::Delivered).unwrap();
    assert_eq!(Arc::strong_count(&server.inner), before);
}

#[test]
fn runtime_close_blocks_all_servers_immediately() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
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

    let (source_a, sink_a, _) = publishing_source(Some(0.0f64));
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
    while server_a.inner.enter_operation().is_ok() {
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, sink, disconnected) = publishing_source(Some(10.0));
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

    runtime
        .claim_server(server_b.inner.generation, id_b)
        .unwrap();

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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let (source_a, _, _) = publishing_source(Some(0.0f64));
    let prep_a = runtime
        .prepare(&source_a, RtdTopic::single("a-0").unwrap())
        .unwrap();
    let id_a = prep_a.id();
    prep_a.commit();
    let conn_a = runtime
        .connect_transaction(&server, TopicId(1), id_a)
        .unwrap();
    conn_a.commit().unwrap();

    let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
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
fn server_standalone_termination() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
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

    let server_a_clone = server_a.clone();
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source_a, sink_a, _) = publishing_source(Some(0.0f64));
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

    let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
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
    let runtime = Arc::new(SubscriptionRuntime::with_limits(limits));
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let mut sinks_a = Vec::new();
    for i in 0..6 {
        let (source, sink, _) = publishing_source(Some(0.0f64));
        let prep = runtime
            .prepare(&source, RtdTopic::single(format!("a-{}", i)).unwrap())
            .unwrap();
        let id = prep.id();
        prep.commit();
        let conn = runtime
            .connect_transaction(&server_a, TopicId(i), id)
            .unwrap();
        conn.commit().unwrap();
        sinks_a.push(sink.lock().clone().unwrap());
    }

    let mut sinks_b = Vec::new();
    for i in 0..5 {
        let (source, sink, _) = publishing_source(Some(0.0f64));
        let prep = runtime
            .prepare(&source, RtdTopic::single(format!("b-{}", i)).unwrap())
            .unwrap();
        let id = prep.id();
        prep.commit();
        let conn = runtime
            .connect_transaction(&server_b, TopicId(i), id)
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server_a = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let server_b = runtime
        .register_server(ServerGeneration::new(2).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(1.0f64));
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
    let runtime = Arc::new(SubscriptionRuntime::new());
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

    let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
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
    assert!(reg_res.is_ok());
    assert!(runtime.servers.lock().is_empty());
}

#[test]
fn inflight_prepare_waits_for_close() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (enter_tx, enter_rx) = std::sync::mpsc::channel();
    let (unblock_tx, unblock_rx) = std::sync::mpsc::channel();
    let unblock_rx = Arc::new(Mutex::new(unblock_rx));

    runtime.set_operation_enter_hook(Some(Arc::new(move || {
        enter_tx.send(()).unwrap();
        unblock_rx.lock().recv().unwrap();
    })));

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

    let source = RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        DroppingSource(Arc::clone(&source_dropped)),
    )
    .unwrap();
    let runtime_clone = Arc::clone(&runtime);
    let handle_prep = std::thread::spawn(move || {
        runtime_clone.prepare(&source, RtdTopic::single("topic").unwrap())
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
    let prep = prep_res.unwrap();
    drop(prep);

    let catalog = runtime.catalog.lock();
    assert!(catalog.entries.is_empty());
    drop(catalog);

    assert!(source_dropped.load(Ordering::Acquire));
}

#[test]
fn reentrant_drop_safety() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

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

    let source = RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        ReentrantSource {
            runtime: Arc::clone(&runtime),
        },
    )
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(0.0f64));
    let prep = runtime
        .prepare(&source, RtdTopic::single("test").unwrap())
        .unwrap();
    let id = prep.id();

    server.inner.publish.mark_closing_for_test();

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
impl RtdSubscription for FailingDisconnectSubscription {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        Arc::new(RtdCancellationHandle::noop())
    }
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TEST_SENTINEL,
        })
    }
}

#[test]
fn server_terminate_returns_cleanup_error_to_caller_and_waiter() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(0.0f64));
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
            .inner
            .subscriptions
            .lock()
            .insert(TopicId(1), Box::new(FailingDisconnectSubscription));
    }

    let server_clone = server.clone();
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

    let server_clone = std::sync::Arc::clone(&server.inner);
    let admission = server_clone.begin_termination();
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

    let server_clone = std::sync::Arc::clone(&server.inner);
    let admission = server_clone.begin_termination();
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(0.0f64));
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
            .inner
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(0.0f64));
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
            .inner
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
impl RtdSubscription for PanickingCancelSubscription {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        Arc::new(RtdCancellationHandle::new(|| {
            panic!("request_cancel panic test");
        }))
    }
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        Ok(())
    }
}

#[test]
fn request_cancel_panic_propagates_to_termination() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(0.0f64));
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
            .inner
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let (tx_close, rx_close) = std::sync::mpsc::channel();
    let (tx_enter, rx_enter) = std::sync::mpsc::channel();

    let source = RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        DelayedSubscribeFailingSource {
            tx_entered: std::sync::Mutex::new(Some(tx_enter)),
            rx_close: Mutex::new(rx_close),
        },
    )
    .unwrap();
    let prep = runtime
        .prepare(&source, RtdTopic::single("delayed_fail").unwrap())
        .unwrap();
    let id = prep.id();
    prep.commit();

    let runtime_clone = Arc::clone(&runtime);
    let server_clone = server.clone();
    let id_clone = id;

    let handle = std::thread::spawn(move || {
        runtime_clone.connect_transaction(&server_clone, TopicId(1), id_clone)
    });

    rx_enter.recv().unwrap();

    server.inner.publish.mark_closing_for_test();

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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source, _, _) = publishing_source::<f64>(None);
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
    let runtime = Arc::new(SubscriptionRuntime::new());

    let (source_a, _, _) = publishing_source::<f64>(None);
    let (source_b, _, _) = publishing_source::<f64>(None);
    let topic = RtdTopic::single("shared").unwrap();

    let first = runtime.prepare(&source_a, topic.clone()).unwrap();
    let second = runtime.prepare(&source_b, topic).unwrap();

    assert_ne!(first.key(), second.key());

    first.rollback();
    second.rollback();
}

#[test]
fn same_handle_reuses_active_subscription_identity() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(1.0_f64));
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
    let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
        max_source_ids: RtdCapacity::from_usize(1),
        ..RtdLimits::standard()
    }));

    let (first_source, _, _) = publishing_source::<f64>(None);

    runtime
        .prepare(&first_source, RtdTopic::single("first").unwrap())
        .unwrap()
        .rollback();
    drop(first_source);

    let (second_source, _, _) = publishing_source::<f64>(None);

    runtime
        .prepare(&second_source, RtdTopic::single("second").unwrap())
        .expect("a released source identity returns to the live quota")
        .rollback();
}

#[test]
fn live_source_reuses_identity_after_pending_subscription_is_removed() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source, _, _) = publishing_source::<f64>(None);
    let topic = RtdTopic::single("stable").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();

    let first_key = *first.key();
    first.rollback();

    let second = runtime.prepare(&source, topic).unwrap();

    assert_ne!(second.key(), &first_key);
}

#[test]
fn failed_pending_admission_rolls_back_new_source_identity() {
    let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
        max_pending: RtdCapacity::disabled(),
        max_source_ids: RtdCapacity::from_usize(1),
        ..RtdLimits::standard()
    }));

    let (source, _, _) = publishing_source::<f64>(None);

    assert!(matches!(
        runtime.prepare(&source, RtdTopic::single("blocked").unwrap()),
        Err(XllError::Overloaded)
    ));

    assert_eq!(runtime.catalog.lock().identities.distinct_source_count(), 0);
}

#[test]
fn source_refcount_tracks_live_subscription_identities() {
    let mut index = SubscriptionIdentityIndex::default();
    let (source, _, _) = publishing_source::<f64>(None);
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
    let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
        max_source_ids: RtdCapacity::from_usize(1),
        ..RtdLimits::standard()
    }));
    let (source, _, _) = publishing_source::<f64>(None);

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
    let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
        max_source_ids: RtdCapacity::from_usize(1),
        ..RtdLimits::standard()
    }));
    let (source_a, _, _) = publishing_source::<f64>(None);
    let (source_b, _, _) = publishing_source::<f64>(None);

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
    let (source, _, _) = publishing_source::<f64>(None);
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source, _, _) = publishing_source::<f64>(None);

    let topic_a = RtdTopic::new(["a\0b", "c"]).unwrap();
    let topic_b = RtdTopic::new(["a", "b\0c"]).unwrap();

    let prepared_a = runtime.prepare(&source, topic_a).unwrap();

    let prepared_b = runtime.prepare(&source, topic_b).unwrap();

    assert_ne!(prepared_a.key(), prepared_b.key());
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn structurally_equal_topics_share_transport_key() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source, _, _) = publishing_source::<f64>(None);

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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source, _, _) = publishing_source::<f64>(None);

    let topic = RtdTopic::single("x".repeat(16 * 1024)).unwrap();
    let prepared = runtime.prepare(&source, topic).unwrap();

    let transport = prepared.key().to_transport();
    assert_eq!(transport.encode_utf16().count(), 43);
    assert!(transport.starts_with("stream:v1:"));
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn distinct_identities_receive_distinct_transport_keys() {
    let runtime = Arc::new(SubscriptionRuntime::new());

    let (source_a, _, _) = publishing_source::<f64>(None);
    let (source_b, _, _) = publishing_source::<f64>(None);

    let topic = RtdTopic::single("same").unwrap();

    let a = runtime.prepare(&source_a, topic.clone()).unwrap();
    let b = runtime.prepare(&source_b, topic).unwrap();

    assert_ne!(a.key(), b.key());
    runtime.catalog.lock().assert_identity_invariants();
}

#[test]
fn identity_index_is_removed_after_final_unbind() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let (source, _, _) = publishing_source(Some(1.0_f64));

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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let (source_a, _, _) = publishing_source::<f64>(None);
    let (source_b, _, _) = publishing_source::<f64>(None);

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
    let runtime = Arc::new(SubscriptionRuntime::new());
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

    let (source, sink, _) = publishing_source(Some(0.0f64));
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
    let runtime = Arc::new(SubscriptionRuntime::new());
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

    let (source, sink, _) = publishing_source(Some(0.0f64));
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let state = Arc::new(TestNotifierState::new());
    state.outcomes.lock().push_back(TestNotifyOutcome::Panic);

    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&state)))
        .unwrap();

    let (source, sink, _) = publishing_source(Some(0.0f64));
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
fn parent_runtime_drop_causes_fail_closed_on_server() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    drop(runtime);

    assert!(matches!(
        server.inner.enter_operation(),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        server.inner.enter_owned_operation(),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        server.inner.publish.publish(
            TopicId(1),
            ConnectionGeneration::new(1).unwrap(),
            RtdValue::Number(1.0).into_stored().unwrap(),
        ),
        Err(XllError::Closing)
    ));
}

#[test]
fn runtime_close_and_publish_race() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let (source, sink, _) = publishing_source(Some(0.0f64));
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
fn quota_permit_survives_parent_drop_and_releases_on_drain() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let quota = triomphe::Arc::clone(&runtime.queued_update_quota);
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    let (source, sink_slot, _) = publishing_source(Some(0.0f64));
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
    assert_eq!(quota.used(), 1);

    drop(runtime);
    assert_eq!(quota.used(), 1);

    drop(sink);
    drop(sink_slot);
    drop(server);
    assert_eq!(quota.used(), 0);
}

pub(crate) struct SinkHoldingSubscription<T> {
    _sink: RtdSink<T>,
}

impl<T: Send + 'static> RtdSubscription for SinkHoldingSubscription<T> {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        Arc::new(RtdCancellationHandle::noop())
    }
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();
    assert_eq!(triomphe::Arc::count(&server.inner.publish), 1);

    let source = RtdSourceHandle::for_internal(
        crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
        SinkCapturingSource,
    )
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

    // While active, subscription holds a sink reference to PublishCore
    assert_eq!(triomphe::Arc::count(&server.inner.publish), 2);

    // Terminate server, closing and dropping subscriptions
    server.terminate().unwrap();

    // After termination, subscription sink is dropped, restoring unique ownership to SubscriptionServer
    assert_eq!(triomphe::Arc::count(&server.inner.publish), 1);
    assert!(triomphe::Arc::is_unique(&server.inner.publish));
}

#[test]
fn prepare_warm_path_reuses_registered_source_identity() {
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(1.0_f64));
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
    // The pending catalog entry is consumed/removed, so the runtime no longer retains Arc<Source>.
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
    let runtime = Arc::new(SubscriptionRuntime::new());
    let server = runtime
        .register_server(ServerGeneration::new(1).expect("non-zero test server generation"))
        .unwrap();

    let (source, _, _) = publishing_source(Some(1.0_f64));
    let topic = RtdTopic::single("existing-active-noop").unwrap();

    let first = runtime.prepare(&source, topic.clone()).unwrap();
    let id = first.id();
    let key = *first.key();
    first.commit();

    let conn = runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap();
    conn.commit().unwrap();

    // Record baseline weak count of runtime (from server, etc.)
    let baseline_weak = Arc::weak_count(&runtime);

    // Prepare on existing active: must NOT downgrade runtime (no weak count bump)
    let warm = runtime.prepare(&source, topic).unwrap();
    assert_eq!(warm.id(), id);
    assert_eq!(warm.key(), &key);
    assert!(!warm.has_reservation());
    assert_eq!(Arc::weak_count(&runtime), baseline_weak);

    // Rollback is a no-op: catalog active keys and pending are untouched
    warm.rollback();
    assert_eq!(Arc::weak_count(&runtime), baseline_weak);
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
    let runtime_a = Arc::new(SubscriptionRuntime::new());
    let runtime_b = Arc::new(SubscriptionRuntime::new());

    let (source, _, _) = publishing_source::<f64>(None);
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
