use super::{RtdSink, RtdSource, RtdSubscription, SourceRegistration};
use crate::generation::RuntimeGeneration;
use crate::subscription::{RtdTopic, TopicId};
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
    Stop(usize),
    Cancel(usize),
    Disconnect(usize),
    Join(usize),
}

#[derive(Clone, Copy)]
enum Failure {
    None,
    StopPanic,
    JoinError,
    JoinPanic,
}

struct LifecycleSource {
    id: usize,
    events: Arc<Mutex<Vec<Event>>>,
    failure: Failure,
}

struct LifecycleSubscription {
    id: usize,
    events: Arc<Mutex<Vec<Event>>>,
    sink: Option<RtdSink<i32>>,
}

// SAFETY: the only retained sink belongs to this subscription and is removed
// before disconnect completes; no worker or callback can use it.
unsafe impl RtdSubscription for LifecycleSubscription {
    fn request_cancel(&self) {
        self.events.lock().push(Event::Cancel(self.id));
    }

    fn disconnect_and_wait(mut self) -> XllResult<()> {
        self.sink.take();
        self.events.lock().push(Event::Disconnect(self.id));
        Ok(())
    }
}

// SAFETY: subscribe transfers the sink only to its unique subscription owner.
// Lifecycle hooks own no source worker, so even injected panics preserve the
// source-worker barrier.
unsafe impl RtdSource for LifecycleSource {
    type Value = i32;
    type Subscription = LifecycleSubscription;

    fn subscribe(&self, _topic: &RtdTopic, sink: RtdSink<i32>) -> XllResult<Self::Subscription> {
        Ok(LifecycleSubscription {
            id: self.id,
            events: Arc::clone(&self.events),
            sink: Some(sink),
        })
    }

    fn request_stop(&self) {
        self.events.lock().push(Event::Stop(self.id));
        if matches!(self.failure, Failure::StopPanic) {
            panic!("injected source cancellation panic");
        }
    }

    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.events.lock().push(Event::Join(self.id));
        match self.failure {
            Failure::JoinError => Err(XllError::Overloaded),
            Failure::JoinPanic => panic!("injected source shutdown panic"),
            Failure::None | Failure::StopPanic => Ok(()),
        }
    }
}

#[test]
fn runtime_cancels_all_sources_and_disconnects_before_joining_shared_workers() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
    let mut handles = Vec::new();
    for id in 0..3 {
        handles.push(
            registration
                .register(LifecycleSource {
                    id,
                    events: Arc::clone(&events),
                    failure: Failure::None,
                })
                .unwrap(),
        );
    }
    let runtime =
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish());
    let server = runtime.register_test_server(1);
    for (index, source) in handles.iter().enumerate() {
        let prepared = runtime
            .prepare(source, RtdTopic::single("shared").unwrap().borrowed())
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, TopicId(index as i32 + 1), id)
            .unwrap()
            .commit()
            .unwrap();
    }

    runtime.close().unwrap();
    let observed = events.lock().clone();
    assert_eq!(
        &observed[..3],
        &[Event::Stop(0), Event::Stop(1), Event::Stop(2)]
    );
    let first_join = observed
        .iter()
        .position(|event| matches!(event, Event::Join(_)))
        .unwrap();
    for id in 0..3 {
        assert!(observed[..first_join].contains(&Event::Disconnect(id)));
    }
    assert_eq!(
        &observed[first_join..],
        &[Event::Join(0), Event::Join(1), Event::Join(2)]
    );
    runtime.close().unwrap();
    assert_eq!(*events.lock(), observed);
}

#[test]
fn source_lifecycle_failures_do_not_skip_later_cancellation_or_join_hooks() {
    for failure in [Failure::StopPanic, Failure::JoinError, Failure::JoinPanic] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
        for id in 0..3 {
            registration
                .register(LifecycleSource {
                    id,
                    events: Arc::clone(&events),
                    failure: if id == 0 { failure } else { Failure::None },
                })
                .unwrap();
        }
        let runtime = crate::subscription::SubscriptionRuntime::with_sources_for_internal(
            registration.finish(),
        );

        let result = runtime.close();
        match failure {
            Failure::JoinError => assert!(matches!(result, Err(XllError::Overloaded))),
            _ => assert!(matches!(result, Err(XllError::Panic))),
        }
        assert_eq!(
            *events.lock(),
            vec![
                Event::Stop(0),
                Event::Stop(1),
                Event::Stop(2),
                Event::Join(0),
                Event::Join(1),
                Event::Join(2),
            ],
        );
        assert!(runtime.close().is_err());
        assert_eq!(events.lock().len(), 6);
    }
}

struct StopSignal {
    stopped: Mutex<bool>,
    changed: Condvar,
}

struct StopDependentSource {
    signal: Arc<StopSignal>,
    entered: std::sync::mpsc::SyncSender<()>,
}

struct StopDependentSubscription {
    signal: Arc<StopSignal>,
    entered: std::sync::mpsc::SyncSender<()>,
}

// SAFETY: this fixture never retains a sink or creates a worker. The delayed
// cleanup models waiting for a shared source cancellation notification only.
unsafe impl RtdSubscription for StopDependentSubscription {
    fn request_cancel(&self) {}

    fn disconnect_and_wait(self) -> XllResult<()> {
        self.entered.send(()).unwrap();
        let mut stopped = self.signal.stopped.lock();
        self.signal.changed.wait_while_for(
            &mut stopped,
            |stopped| !*stopped,
            Duration::from_secs(5),
        );
        if *stopped {
            Ok(())
        } else {
            // A timeout turns an ordering regression into a bounded failure.
            Err(XllError::Overloaded)
        }
    }
}

// SAFETY: subscribe does not retain its supplied sink. Source shutdown owns
// no background execution and its cancellation is synchronized with cleanup.
unsafe impl RtdSource for StopDependentSource {
    type Value = i32;
    type Subscription = StopDependentSubscription;

    fn subscribe(&self, _topic: &RtdTopic, _sink: RtdSink<i32>) -> XllResult<Self::Subscription> {
        Ok(StopDependentSubscription {
            signal: Arc::clone(&self.signal),
            entered: self.entered.clone(),
        })
    }

    fn request_stop(&self) {
        *self.signal.stopped.lock() = true;
        self.signal.changed.notify_all();
    }
}

#[test]
fn source_cancellation_precedes_waiting_for_deferred_disconnect_admission() {
    let signal = Arc::new(StopSignal {
        stopped: Mutex::new(false),
        changed: Condvar::new(),
    });
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
    let source = registration
        .register(StopDependentSource {
            signal,
            entered: entered_tx,
        })
        .unwrap();
    let runtime =
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish());
    let server = runtime.register_test_server(1);
    let prepared = runtime
        .prepare(&source, RtdTopic::single("shared").unwrap().borrowed())
        .unwrap();
    let id = prepared.id();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    server
        .disconnect_deferred(TopicId(1), Box::new(()))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    runtime.close().unwrap();
}

#[test]
fn empty_source_arena_does_not_prepare_a_source_join_worker() {
    let runtime = crate::subscription::SubscriptionRuntime::<()>::new();
    runtime
        .fail_source_shutdown_preflight
        .store(true, std::sync::atomic::Ordering::Release);
    runtime.close().unwrap();
    assert!(
        runtime
            .fail_source_shutdown_preflight
            .load(std::sync::atomic::Ordering::Acquire)
    );
}

#[test]
fn miri_static_catalog_stops_every_source_before_join_despite_each_failure() {
    for failure in [
        Failure::None,
        Failure::StopPanic,
        Failure::JoinError,
        Failure::JoinPanic,
    ] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let source = |id| LifecycleSource {
            id,
            events: Arc::clone(&events),
            failure: if id == 0 { failure } else { Failure::None },
        };
        let registration = SourceRegistration::<
            LifecycleSubscription,
            (LifecycleSource, LifecycleSource, LifecycleSource),
        >::new(RuntimeGeneration::new(1).unwrap());
        let (a, b, c) = registration
            .register_sources((source(0), source(1), source(2)))
            .unwrap();
        let runtime = crate::subscription::SubscriptionRuntime::with_sources_for_internal(
            registration.finish(),
        );
        let server = runtime.register_test_server(1);
        for (index, source) in [a, b, c].iter().enumerate() {
            let prepared = runtime
                .prepare(
                    source,
                    RtdTopic::single("static lifecycle").unwrap().borrowed(),
                )
                .unwrap();
            let id = prepared.id();
            prepared.commit();
            runtime
                .connect_transaction(&server, TopicId(index as i32 + 1), id)
                .unwrap()
                .commit()
                .unwrap();
        }
        let result = runtime.close();
        match failure {
            Failure::None => result.unwrap(),
            Failure::JoinError => assert!(matches!(result, Err(XllError::Overloaded))),
            Failure::StopPanic | Failure::JoinPanic => {
                assert!(matches!(result, Err(XllError::Panic)))
            }
        }
        let observed = events.lock().clone();
        assert_eq!(
            &observed[..3],
            &[Event::Stop(0), Event::Stop(1), Event::Stop(2)]
        );
        let first_join = observed
            .iter()
            .position(|event| matches!(event, Event::Join(_)))
            .unwrap();
        for id in 0..3 {
            assert!(observed[..first_join].contains(&Event::Cancel(id)));
            assert!(observed[..first_join].contains(&Event::Disconnect(id)));
        }
        assert_eq!(
            &observed[first_join..],
            &[Event::Join(0), Event::Join(1), Event::Join(2)]
        );
        let _ = runtime.close();
        assert_eq!(*events.lock(), observed);
    }
}

#[test]
fn static_catalog_rejects_repeated_registration_and_foreign_source_ids() {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration =
        SourceRegistration::<LifecycleSubscription, (LifecycleSource,)>::new(generation);
    let source = || LifecycleSource {
        id: 0,
        events: Arc::new(Mutex::new(Vec::new())),
        failure: Failure::None,
    };
    let (handle,) = registration.register_sources((source(),)).unwrap();
    assert!(registration.register_sources((source(),)).is_err());
    let arena = registration.finish();
    assert!(arena.resolve(handle.id).is_some());
    let other = SourceRegistration::<LifecycleSubscription, (LifecycleSource,)>::new(generation);
    let (foreign,) = other.register_sources((source(),)).unwrap();
    assert_eq!(foreign.id.sequence, handle.id.sequence);
    assert_eq!(foreign.id.generation, handle.id.generation);
    assert!(arena.resolve(foreign.id).is_none());
    for (generation, sequence) in [
        (RuntimeGeneration::new(2).unwrap(), 1),
        (generation, 0),
        (generation, 2),
        (generation, u64::MAX),
    ] {
        assert!(
            arena
                .resolve(super::SourceHandleId {
                    generation,
                    registration: handle.id.registration,
                    sequence
                })
                .is_none()
        );
    }
    let unregistered =
        SourceRegistration::<LifecycleSubscription, (LifecycleSource,)>::new(generation).finish();
    assert!(unregistered.is_empty());
    unregistered.request_stop().unwrap();
    unregistered.shutdown_and_wait().unwrap();
}

#[test]
fn erased_sources_share_identity_without_weakening_generation_or_shutdown_barriers() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration = SourceRegistration::<crate::BoxedRtdSubscription>::new(generation);
    let source = || LifecycleSource {
        id: 0,
        events: Arc::clone(&events),
        failure: Failure::None,
    };
    let handle: super::RtdSourceHandle = registration.register_erased(source()).unwrap();
    let other = registration.register_erased(source()).unwrap();
    assert_ne!(handle.id, other.id);
    let runtime =
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish());
    let server = runtime.register_test_server(1);
    let topic = RtdTopic::single("erased").unwrap();
    let prepared = runtime.prepare(&handle, topic.borrowed()).unwrap();
    let id = prepared.id();
    let alias = handle.erase();
    let same = runtime.prepare(&alias, topic.borrowed()).unwrap();
    assert_eq!(same.id(), id);
    same.rollback();
    prepared.commit();
    runtime
        .connect_transaction(&server, TopicId(1), id)
        .unwrap()
        .commit()
        .unwrap();
    // A different add-in may have the same lifecycle generation and source
    // sequence. Registration identity must still prevent cross-runtime use.
    let foreign = SourceRegistration::<crate::BoxedRtdSubscription>::new(generation)
        .register_erased(source())
        .unwrap();
    assert!(matches!(
        runtime.prepare(&foreign, topic.borrowed()),
        Err(XllError::StaleHandle)
    ));
    runtime.close().unwrap();
    let events = events.lock();
    let disconnect = events
        .iter()
        .position(|event| *event == Event::Disconnect(0))
        .unwrap();
    assert!(events[..disconnect].contains(&Event::Cancel(0)));
    assert!(events[disconnect + 1..].contains(&Event::Join(0)));
}

#[test]
fn typed_source_handle_erasure_preserves_catalog_identity() {
    let generation = RuntimeGeneration::new(1).unwrap();
    let registration =
        SourceRegistration::<LifecycleSubscription, (LifecycleSource,)>::new(generation);
    let (typed,) = registration
        .register_sources((LifecycleSource {
            id: 0,
            events: Arc::new(Mutex::new(Vec::new())),
            failure: Failure::None,
        },))
        .unwrap();
    let erased: super::RtdSourceHandle = typed.erase();
    assert_eq!(typed.id, erased.id);
    let runtime =
        crate::subscription::SubscriptionRuntime::with_sources_for_internal(registration.finish());
    let topic = RtdTopic::single("typed-erased").unwrap();
    let typed_prepared = runtime.prepare(&typed, topic.borrowed()).unwrap();
    let erased_prepared = runtime.prepare(&erased, topic.borrowed()).unwrap();
    assert_eq!(typed_prepared.id(), erased_prepared.id());
    typed_prepared.rollback();
    erased_prepared.rollback();
    runtime.close().unwrap();
}

#[test]
fn exhausted_source_registration_identity_fails_before_retaining_sources() {
    let mut dynamic =
        SourceRegistration::<crate::BoxedRtdSubscription>::new(RuntimeGeneration::new(1).unwrap());
    dynamic.registration_id = None;
    let source = || LifecycleSource {
        id: 0,
        events: Arc::new(Mutex::new(Vec::new())),
        failure: Failure::None,
    };
    assert!(dynamic.register_erased(source()).is_err());
    assert!(dynamic.finish().is_empty());
    let mut typed = SourceRegistration::<LifecycleSubscription, (LifecycleSource,)>::new(
        RuntimeGeneration::new(1).unwrap(),
    );
    typed.registration_id = None;
    assert!(typed.register_sources((source(),)).is_err());
    assert!(typed.finish().is_empty());
}
