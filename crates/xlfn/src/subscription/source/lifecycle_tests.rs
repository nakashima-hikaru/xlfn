use super::{RtdSink, RtdSource, RtdSubscription, SourceRegistration};
use crate::generation::RuntimeGeneration;
use crate::subscription::{RtdTopic, SubscriptionRuntime, TopicId};
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

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
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
    let runtime = SubscriptionRuntime::with_sources_for_internal(registration.finish());
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
        let runtime = SubscriptionRuntime::with_sources_for_internal(registration.finish());

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

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
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
    let runtime = SubscriptionRuntime::with_sources_for_internal(registration.finish());
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
    let runtime = SubscriptionRuntime::new();
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
