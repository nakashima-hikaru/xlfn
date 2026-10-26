use super::*;
use crate::excel_rtd::RtdSubscriptionHost;
use crate::generation::RuntimeGeneration;
use crate::subscription::{
    RefreshOutcome, RtdCapacity, RtdLimits, RtdSendError, RtdSourceHandle, SourceRegistration,
    StoredRtdValue, SubscriptionRuntime, SubscriptionServerHandle, TopicId,
};
use std::collections::BTreeMap;
use std::sync::Arc as StdArc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::Instant;

const DEADLINE: Duration = Duration::from_secs(10);

fn runtime_with_source<T: IntoRtdValue + Send + 'static>(
    source: RtdMultiplexedSource<T>,
    limits: RtdLimits,
) -> (
    SubscriptionRuntime,
    RtdSourceHandle<RtdMultiplexedSource<T>>,
) {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration = SourceRegistration::new(generation);
    let handle = registration.register(source).unwrap();
    (
        SubscriptionRuntime::with_host(
            generation,
            limits,
            RtdSubscriptionHost::default(),
            registration.finish(),
        ),
        handle,
    )
}

fn connect<S: RtdSource>(
    runtime: &SubscriptionRuntime,
    server: &SubscriptionServerHandle,
    source: &RtdSourceHandle<S>,
    topic_id: i32,
    topic: &str,
) -> XllResult<()> {
    let prepared = runtime.prepare(source, RtdTopic::single(topic)?.borrowed())?;
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(server, TopicId(topic_id), id)?
        .commit()
}

fn refresh_until(
    server: &SubscriptionServerHandle,
    done: impl Fn(&BTreeMap<i32, StoredRtdValue>) -> bool,
) -> BTreeMap<i32, StoredRtdValue> {
    let deadline = Instant::now() + DEADLINE;
    let mut observed = BTreeMap::new();
    loop {
        let batch = server.begin_refresh().unwrap();
        for update in &batch.updates {
            observed.insert(update.topic_id, update.value.clone());
        }
        batch.complete(RefreshOutcome::Delivered).unwrap();
        if done(&observed) {
            return observed;
        }
        assert!(
            Instant::now() < deadline,
            "RTD updates stalled: {observed:?}"
        );
        std::thread::yield_now();
    }
}

fn run_forwarder(
    context: RtdSourceContext<i32>,
    events: mpsc::Sender<RtdSourceEvent<i32>>,
) -> XllResult<()> {
    loop {
        match context.recv_timeout(DEADLINE) {
            Ok(Some(event)) => events.send(event).map_err(|_| XllError::Closing)?,
            Ok(None) => {}
            Err(XllError::Closing) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[test]
fn construction_is_lazy_and_unstarted_shutdown_is_idempotent() {
    let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_started = std::sync::Arc::clone(&started);
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |_| {
        worker_started.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(started.load(Ordering::SeqCst), 0);
    source.shutdown_and_wait().unwrap();
    source.shutdown_and_wait().unwrap();
    assert_eq!(started.load(Ordering::SeqCst), 0);
}

#[test]
#[cfg(not(miri))]
fn one_source_worker_delivers_updates_to_2048_subscriptions() {
    const TOPICS: i32 = 2048;
    let starts = StdArc::new(AtomicUsize::new(0));
    let worker_starts = StdArc::clone(&starts);
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        worker_starts.fetch_add(1, Ordering::SeqCst);
        loop {
            match context.recv_timeout(DEADLINE) {
                Ok(Some(RtdSourceEvent::Subscribe { topic, sender, .. })) => sender
                    .try_send(topic.part(0).unwrap().parse::<i32>().unwrap())
                    .map_err(RtdSendError::into_error)?,
                Ok(_) => {}
                Err(XllError::Closing) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    });
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    let shared = Arc::clone(&source.shared);
    let (runtime, source) = runtime_with_source(
        source,
        RtdLimits::standard().with_max_workers(RtdCapacity::disabled_if_zero(1)),
    );
    let server = runtime.register_test_server(1);
    for id in 1..=TOPICS {
        connect(&runtime, &server, &source, id, &id.to_string()).unwrap();
    }
    let values = refresh_until(&server, |values| values.len() == TOPICS as usize);
    for id in 1..=TOPICS {
        assert_eq!(values[&id], StoredRtdValue::Integer(id));
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(shared.state.lock().active.len(), TOPICS as usize);
    runtime.close().unwrap();
    assert!(shared.state.lock().active.is_empty());
    assert!(shared.state.lock().events.is_empty());
}

#[test]
fn stalled_source_does_not_block_disconnect_and_retains_control_capacity_until_consumed() {
    let read_events = StdArc::new(AtomicBool::new(false));
    let worker_read_events = StdArc::clone(&read_events);
    let (event_tx, event_rx) = mpsc::channel();
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        while !worker_read_events.load(Ordering::Acquire) {
            if context.is_closed() {
                return Ok(());
            }
            std::thread::yield_now();
        }
        run_forwarder(context, event_tx)
    })
    .with_limits(
        RtdSourceLimits::standard().with_max_subscriptions(RtdCapacity::disabled_if_zero(1)),
    );
    let shared = Arc::clone(&source.shared);
    let (runtime, source) = runtime_with_source(source, RtdLimits::standard());
    let runtime = StdArc::new(runtime);
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &source, 1, "same-topic").unwrap();
    let disconnect_runtime = StdArc::clone(&runtime);
    let (disconnected_tx, disconnected_rx) = mpsc::channel();
    let disconnect = std::thread::spawn(move || {
        disconnected_tx
            .send(disconnect_runtime.disconnect(&server, TopicId(1)))
            .unwrap();
    });
    let disconnected = disconnected_rx.recv_timeout(DEADLINE);
    if disconnected.is_err() {
        // Unblock a regressed implementation before reporting the timeout.
        shared.stop();
    }
    disconnect.join().unwrap();
    disconnected
        .expect("per-topic disconnect waited for source I/O")
        .unwrap();
    assert!(!read_events.load(Ordering::Acquire));
    assert_eq!(shared.state.lock().events.len(), 2);
    assert_eq!(shared.state.lock().removals, 1);
    assert!(matches!(
        connect(&runtime, &server, &source, 2, "same-topic"),
        Err(XllError::Overloaded),
    ));

    read_events.store(true, Ordering::Release);
    let RtdSourceEvent::Subscribe {
        id: old_id,
        sender: old_sender,
        ..
    } = event_rx.recv_timeout(DEADLINE).unwrap()
    else {
        panic!("expected first registration")
    };
    assert!(old_sender.is_closed());
    let RtdSourceEvent::Unsubscribe { id } = event_rx.recv_timeout(DEADLINE).unwrap() else {
        panic!("expected reserved removal")
    };
    assert_eq!(old_id, id);
    assert_eq!(shared.state.lock().removals, 0);
    connect(&runtime, &server, &source, 2, "same-topic").unwrap();
    let RtdSourceEvent::Subscribe {
        id: new_id, sender, ..
    } = event_rx.recv_timeout(DEADLINE).unwrap()
    else {
        panic!("expected new registration")
    };
    assert_ne!(old_id, new_id);
    assert!(matches!(
        old_sender.try_send(1),
        Err(RtdSendError::Closed(_))
    ));
    sender.try_send(2).unwrap();
    refresh_until(&server, |values| {
        values.get(&2) == Some(&StoredRtdValue::Integer(2))
    });
    runtime.close().unwrap();
}

#[test]
fn completion_and_failures_close_all_topics_and_apply_terminal_policy() {
    for outcome in 0..3 {
        let finish = StdArc::new(AtomicBool::new(false));
        let worker_finish = StdArc::clone(&finish);
        let (senders_tx, senders_rx) = mpsc::channel();
        let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
            let mut senders = Vec::new();
            while senders.len() != 2 {
                if let Some(RtdSourceEvent::Subscribe { sender, .. }) =
                    context.recv_timeout(DEADLINE)?
                {
                    sender.try_send(7).map_err(RtdSendError::into_error)?;
                    senders.push(sender);
                }
            }
            senders_tx.send(senders).unwrap();
            while !worker_finish.load(Ordering::Acquire) {
                if context.is_closed() {
                    return Ok(());
                }
                std::thread::yield_now();
            }
            match outcome {
                0 => Ok(()),
                1 => Err(XllError::Overloaded),
                _ => panic!("injected multiplexed producer panic"),
            }
        })
        .with_error_policy(RtdProducerErrorPolicy::PublishError(
            crate::ExcelError::NotAvailable,
        ));
        let shared = Arc::clone(&source.shared);
        let (runtime, source) = runtime_with_source(source, RtdLimits::standard());
        let server = runtime.register_test_server(1);
        connect(&runtime, &server, &source, 1, "one").unwrap();
        connect(&runtime, &server, &source, 2, "two").unwrap();
        let senders = senders_rx.recv_timeout(DEADLINE).unwrap();
        finish.store(true, Ordering::Release);
        for sender in &senders {
            assert!(sender.wait_closed(DEADLINE));
            assert!(matches!(sender.try_send(9), Err(RtdSendError::Closed(_))));
        }
        let expected = if outcome == 0 {
            StoredRtdValue::Integer(7)
        } else {
            StoredRtdValue::Error(crate::ExcelError::NotAvailable)
        };
        refresh_until(&server, |values| {
            values.get(&1) == Some(&expected) && values.get(&2) == Some(&expected)
        });
        assert!(shared.closed.load(Ordering::Acquire));
        assert!(matches!(
            connect(&runtime, &server, &source, 3, "after-completion"),
            Err(XllError::Closing),
        ));
        runtime.close().unwrap();
    }
}

#[test]
fn rejected_topic_validation_does_not_start_worker_or_consume_source_slot() {
    let starts = StdArc::new(AtomicUsize::new(0));
    let worker_starts = StdArc::clone(&starts);
    let (events_tx, events_rx) = mpsc::channel();
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        worker_starts.fetch_add(1, Ordering::SeqCst);
        run_forwarder(context, events_tx)
    })
    .with_limits(
        RtdSourceLimits::standard().with_max_subscriptions(RtdCapacity::disabled_if_zero(1)),
    )
    .with_topic_validator(|topic| match topic.part(0) {
        Some("reject") => Err(XllError::Overloaded),
        Some("panic") => panic!("injected topic validator panic"),
        _ => Ok(()),
    });
    let (runtime, source) = runtime_with_source(source, RtdLimits::standard());
    let server = runtime.register_test_server(1);
    assert!(matches!(
        connect(&runtime, &server, &source, 1, "reject"),
        Err(XllError::Overloaded)
    ));
    let panicked = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
        connect(&runtime, &server, &source, 2, "panic")
    }));
    assert!(panicked.is_err());
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    connect(&runtime, &server, &source, 3, "accepted").unwrap();
    assert!(matches!(
        events_rx.recv_timeout(DEADLINE).unwrap(),
        RtdSourceEvent::Subscribe { .. }
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    // The runtime retains the diagnosed validator panic as a cleanup failure.
    assert!(runtime.close().is_err());
}

#[test]
fn miri_sender_survives_runtime_drop_without_live_publication_capability() {
    let (events_tx, events_rx) = mpsc::channel();
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        run_forwarder(context, events_tx)
    });
    let (runtime, source) = runtime_with_source(source, RtdLimits::standard());
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &source, 1, "retained-sender").unwrap();
    let RtdSourceEvent::Subscribe { sender, .. } = events_rx.recv_timeout(DEADLINE).unwrap() else {
        panic!("expected registration")
    };
    sender.try_send(1).unwrap();
    drop(runtime);
    assert!(sender.is_closed());
    assert!(matches!(sender.try_send(2), Err(RtdSendError::Closed(_))));
}

#[test]
fn runtime_worker_limit_is_shared_by_sources_and_released_only_after_join() {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration = SourceRegistration::new(generation);
    let (first_events_tx, first_events_rx) = mpsc::channel();
    let first = registration
        .register(RtdMultiplexedSource::<i32>::new(
            RtdDelivery::Latest,
            move |context| run_forwarder(context, first_events_tx),
        ))
        .unwrap();
    let starts = StdArc::new(AtomicUsize::new(0));
    let worker_starts = StdArc::clone(&starts);
    let (second_events_tx, second_events_rx) = mpsc::channel();
    let second = registration
        .register(RtdMultiplexedSource::<i32>::new(
            RtdDelivery::Latest,
            move |context| {
                worker_starts.fetch_add(1, Ordering::SeqCst);
                run_forwarder(context, second_events_tx)
            },
        ))
        .unwrap();
    let runtime = SubscriptionRuntime::with_host(
        generation,
        RtdLimits::standard().with_max_workers(RtdCapacity::disabled_if_zero(1)),
        RtdSubscriptionHost::default(),
        registration.finish(),
    );
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &first, 1, "first").unwrap();
    first_events_rx.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        connect(&runtime, &server, &second, 2, "second"),
        Err(XllError::Overloaded)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    server.disconnect(TopicId(1)).unwrap();
    first_events_rx.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        connect(&runtime, &server, &second, 2, "second"),
        Err(XllError::Overloaded)
    ));

    // A source-level join releases the shared worker reservation; per-topic
    // disconnect deliberately leaves that worker available for other topics.
    runtime
        .sources
        .resolve(first.id)
        .unwrap()
        .shutdown_and_wait()
        .unwrap();
    connect(&runtime, &server, &second, 2, "second").unwrap();
    second_events_rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    runtime.close().unwrap();
}

#[test]
fn source_join_preflight_failure_retains_worker_owner_and_is_terminal_for_close() {
    let (events_tx, events_rx) = mpsc::channel();
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        run_forwarder(context, events_tx)
    });
    let (runtime, source) = runtime_with_source(
        source,
        RtdLimits::standard().with_max_workers(RtdCapacity::disabled_if_zero(1)),
    );
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &source, 1, "preflight").unwrap();
    let RtdSourceEvent::Subscribe { sender, .. } = events_rx.recv_timeout(DEADLINE).unwrap() else {
        panic!("expected registration")
    };
    runtime
        .fail_source_shutdown_preflight
        .store(true, Ordering::Release);
    assert!(matches!(runtime.close(), Err(XllError::Overloaded)));
    assert!(sender.is_closed());
    assert!(matches!(
        runtime.termination_coordinator.state.lock().phase,
        crate::subscription::server::ServerTerminationPhase::Failed,
    ));
    assert!(ChannelBudget::try_worker(&runtime.services.channel_budget).is_err());
    assert!(matches!(runtime.close(), Err(XllError::Overloaded)));
    assert!(ChannelBudget::try_worker(&runtime.services.channel_budget).is_err());

    // The unused dispatch closure did not consume the source's unique worker
    // owner. A direct test-only cleanup joins it and releases its reservation.
    runtime.sources.shutdown_and_wait().unwrap();
    drop(ChannelBudget::try_worker(&runtime.services.channel_budget).unwrap());
}

#[test]
fn close_racing_admitted_setup_prevents_late_worker_start_and_revokes_partial_channel() {
    let starts = StdArc::new(AtomicUsize::new(0));
    let worker_starts = StdArc::clone(&starts);
    let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |_| {
        worker_starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let shared = Arc::clone(&source.shared);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let source = source.with_topic_validator(move |_| {
        entered_tx.send(()).unwrap();
        let deadline = Instant::now() + DEADLINE;
        while !shared.closed.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        stopped_tx
            .send(shared.closed.load(Ordering::Acquire))
            .unwrap();
        Ok(())
    });
    let (runtime, source) = runtime_with_source(source, RtdLimits::standard());
    let runtime = StdArc::new(runtime);
    let server = runtime.register_test_server(1);
    let connecting_runtime = StdArc::clone(&runtime);
    let connecting =
        std::thread::spawn(move || connect(&connecting_runtime, &server, &source, 1, "racing"));
    entered_rx.recv_timeout(DEADLINE).unwrap();
    runtime.close().unwrap();
    assert!(stopped_rx.recv_timeout(DEADLINE).unwrap());
    assert!(matches!(connecting.join().unwrap(), Err(XllError::Closing)));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.services.active_quota.used(), 0);
    drop(ChannelBudget::try_worker(&runtime.services.channel_budget).unwrap());
}

#[test]
fn blocking_and_multiplexed_adapters_share_the_runtime_worker_budget() {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration = SourceRegistration::new(generation);
    let (blocking_tx, blocking_rx) = mpsc::channel();
    let blocking = registration
        .register(super::super::channel::RtdChannelSource::new(
            std::num::NonZeroUsize::new(1).unwrap(),
            move |_| {
                let blocking_tx = blocking_tx.clone();
                Ok(move |sender: RtdSender<i32>| {
                    blocking_tx.send(sender.clone()).unwrap();
                    while !sender.is_closed() {
                        std::thread::yield_now();
                    }
                    Ok(())
                })
            },
        ))
        .unwrap();
    let (events_tx, events_rx) = mpsc::channel();
    let multiplexed = registration
        .register(RtdMultiplexedSource::<i32>::new(
            RtdDelivery::Latest,
            move |context| run_forwarder(context, events_tx),
        ))
        .unwrap();
    let runtime = SubscriptionRuntime::with_host(
        generation,
        RtdLimits::standard().with_max_workers(RtdCapacity::disabled_if_zero(1)),
        RtdSubscriptionHost::default(),
        registration.finish(),
    );
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &blocking, 1, "blocking").unwrap();
    let blocking_sender = blocking_rx.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        connect(&runtime, &server, &multiplexed, 2, "multiplexed"),
        Err(XllError::Overloaded),
    ));
    server.disconnect(TopicId(1)).unwrap();
    assert!(blocking_sender.is_closed());
    connect(&runtime, &server, &multiplexed, 2, "multiplexed").unwrap();
    events_rx.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        connect(&runtime, &server, &blocking, 3, "blocking-again"),
        Err(XllError::Overloaded),
    ));
    runtime.close().unwrap();
}

#[test]
fn first_registration_exists_before_an_immediately_completing_producer_runs() {
    for outcome in 0..3 {
        let (_runtime, server, sink) =
            crate::subscription::tests::connected_sink::<i32>(None, "immediate");
        let (observed_tx, observed_rx) = mpsc::channel();
        let source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
            let state = context.shared.state.lock();
            let registered = state.active.len() == 1
                && matches!(state.events.front(), Some(ControlEvent::Subscribe { .. }));
            drop(state);
            observed_tx.send(registered).unwrap();
            match outcome {
                0 => Ok(()),
                1 => Err(XllError::Overloaded),
                _ => panic!("immediate source initialization failure"),
            }
        })
        .with_error_policy(RtdProducerErrorPolicy::PublishError(
            crate::ExcelError::NotAvailable,
        ));
        let subscription = source
            .subscribe(&RtdTopic::single("immediate").unwrap(), sink)
            .expect("a completed first producer must not race away its registration");
        let sender = subscription.channel.sender::<i32>();
        assert!(observed_rx.recv_timeout(DEADLINE).unwrap());
        assert!(sender.wait_closed(DEADLINE));
        assert!(matches!(sender.try_send(3), Err(RtdSendError::Closed(_))));
        if outcome != 0 {
            refresh_until(&server, |values| {
                values.get(&1) == Some(&StoredRtdValue::Error(crate::ExcelError::NotAvailable))
            });
        } else {
            assert_eq!(server.pending_update_count(), 0);
        }
        Box::new(subscription).disconnect_and_wait().unwrap();
        source.shutdown_and_wait().unwrap();
    }
}

#[test]
fn initial_worker_budget_rejection_returns_the_only_source_slot_for_retry() {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration = SourceRegistration::new(generation);
    let (first_events_tx, first_events_rx) = mpsc::channel();
    let first = registration
        .register(RtdMultiplexedSource::<i32>::new(
            RtdDelivery::Latest,
            move |context| run_forwarder(context, first_events_tx),
        ))
        .unwrap();
    let (second_events_tx, second_events_rx) = mpsc::channel();
    let second_source = RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |context| {
        run_forwarder(context, second_events_tx)
    })
    .with_limits(
        RtdSourceLimits::standard().with_max_subscriptions(RtdCapacity::disabled_if_zero(1)),
    );
    let second_shared = Arc::clone(&second_source.shared);
    let second = registration.register(second_source).unwrap();
    let runtime = SubscriptionRuntime::with_host(
        generation,
        RtdLimits::standard().with_max_workers(RtdCapacity::disabled_if_zero(1)),
        RtdSubscriptionHost::default(),
        registration.finish(),
    );
    let server = runtime.register_test_server(1);
    connect(&runtime, &server, &first, 1, "first").unwrap();
    first_events_rx.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        connect(&runtime, &server, &second, 2, "second"),
        Err(XllError::Overloaded)
    ));
    assert!(second_shared.state.lock().active.is_empty());
    assert!(second_shared.state.lock().events.is_empty());
    assert_eq!(second_shared.state.lock().removals, 0);
    server.disconnect(TopicId(1)).unwrap();
    first_events_rx.recv_timeout(DEADLINE).unwrap();
    runtime
        .sources
        .resolve(first.id)
        .unwrap()
        .shutdown_and_wait()
        .unwrap();
    connect(&runtime, &server, &second, 2, "second").unwrap();
    second_events_rx.recv_timeout(DEADLINE).unwrap();
    runtime.close().unwrap();
}

#[test]
fn spawn_failure_preserves_capture_until_disposal_outside_source_mutexes() {
    struct Capture {
        source: std::sync::Weak<RtdMultiplexedSource<i32>>,
        drops: StdArc<AtomicUsize>,
        locks_available: StdArc<AtomicBool>,
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            let source = self.source.upgrade().expect("test source remains owned");
            self.locks_available.store(
                source.worker.try_lock().is_some() && source.shared.state.try_lock().is_some(),
                Ordering::Release,
            );
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    let drops = StdArc::new(AtomicUsize::new(0));
    let locks_available = StdArc::new(AtomicBool::new(false));
    let source = StdArc::new_cyclic(|weak| {
        let capture = Capture {
            source: std::sync::Weak::clone(weak),
            drops: StdArc::clone(&drops),
            locks_available: StdArc::clone(&locks_available),
        };
        RtdMultiplexedSource::<i32>::new(RtdDelivery::Latest, move |_| {
            drop(capture);
            Ok(())
        })
    });
    let (_runtime, _server, sink) =
        crate::subscription::tests::connected_sink::<i32>(None, "spawn-failure");
    source.fail_spawn.store(true, Ordering::Release);
    let result = source.subscribe(&RtdTopic::single("spawn-failure").unwrap(), sink);
    assert!(matches!(result, Err(XllError::Native { .. })));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(source.worker.lock().producer.is_some());
    assert!(source.shared.state.lock().active.is_empty());
    assert!(source.shared.state.lock().events.is_empty());
    source.shutdown_and_wait().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(locks_available.load(Ordering::Acquire));
    source.shutdown_and_wait().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
