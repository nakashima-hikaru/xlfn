use super::*;
use crate::excel_rtd::RtdSubscriptionHost;
use crate::subscription::termination::TerminationPhase;
use crate::subscription::{
    RtdSink, RtdSource, RtdSourceHandle, RtdSubscription, RtdTopic, SourceArena,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Cancel(usize),
    Disconnect(usize),
}

struct Observation {
    expected_cancellations: usize,
    cancellations: AtomicUsize,
    disconnections: AtomicUsize,
    premature_disconnects: AtomicUsize,
    events: Mutex<Vec<Event>>,
}

impl Observation {
    fn new(expected_cancellations: usize) -> Arc<Self> {
        Arc::new(Self {
            expected_cancellations,
            cancellations: AtomicUsize::new(0),
            disconnections: AtomicUsize::new(0),
            premature_disconnects: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
        })
    }

    fn assert_complete(&self) {
        assert_eq!(
            self.cancellations.load(Ordering::Acquire),
            self.expected_cancellations
        );
        assert_eq!(
            self.disconnections.load(Ordering::Acquire),
            self.expected_cancellations
        );
        assert_eq!(self.premature_disconnects.load(Ordering::Acquire), 0);
        let events = self.events.lock();
        assert_eq!(events.len(), self.expected_cancellations * 2);
        assert!(
            events[..self.expected_cancellations]
                .iter()
                .all(|event| matches!(event, Event::Cancel(_)))
        );
        assert!(
            events[self.expected_cancellations..]
                .iter()
                .all(|event| matches!(event, Event::Disconnect(_)))
        );
    }
}

struct ObservedSubscription {
    id: usize,
    observation: Arc<Observation>,
    panic_on_cancel: bool,
}

// SAFETY: the fixture owns no sink or external resource. Shared observations
// remain live through synchronous disconnect and are accessed under synchronization.
unsafe impl RtdSubscription for ObservedSubscription {
    fn request_cancel(&self) {
        self.observation.events.lock().push(Event::Cancel(self.id));
        self.observation
            .cancellations
            .fetch_add(1, Ordering::AcqRel);
        if self.panic_on_cancel {
            panic!("injected termination cancellation panic");
        }
    }

    fn disconnect_and_wait(self) -> XllResult<()> {
        // Report violations outside the framework's panic boundary, which
        // intentionally contains arbitrary source callback panics.
        if self.observation.cancellations.load(Ordering::Acquire)
            < self.observation.expected_cancellations
        {
            self.observation
                .premature_disconnects
                .fetch_add(1, Ordering::AcqRel);
        }
        self.observation
            .events
            .lock()
            .push(Event::Disconnect(self.id));
        self.observation
            .disconnections
            .fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct ObservedSource {
    observation: Arc<Observation>,
}

// SAFETY: this source discards the sink and creates no workers or callbacks.
// Its subscription only records synchronous cancellation/disconnection events.
unsafe impl RtdSource for ObservedSource {
    type Value = i32;
    type Subscription = ObservedSubscription;

    fn subscribe(&self, topic: &RtdTopic, _: RtdSink<i32>) -> XllResult<Self::Subscription> {
        Ok(ObservedSubscription {
            id: topic.part(0).unwrap().parse().unwrap(),
            observation: Arc::clone(&self.observation),
            panic_on_cancel: topic.part(1) == Some("true"),
        })
    }
}

fn observed_runtime(
    observation: &Arc<Observation>,
) -> (
    SubscriptionRuntime<RtdSubscriptionHost, ObservedSubscription>,
    RtdSourceHandle<ObservedSource>,
) {
    let (arena, source) = SourceArena::with_source(
        crate::generation::RuntimeGeneration::new(1).unwrap(),
        ObservedSource {
            observation: Arc::clone(observation),
        },
    )
    .unwrap();
    (
        SubscriptionRuntime::with_sources_for_internal(arena),
        source,
    )
}

fn seed(
    runtime: &SubscriptionRuntime<RtdSubscriptionHost, ObservedSubscription>,
    server: &SubscriptionServerHandle<RtdSubscriptionHost, ObservedSubscription>,
    id: usize,
    source: &RtdSourceHandle<ObservedSource>,
    panic_on_cancel: bool,
) {
    let topic = RtdTopic::new([id.to_string(), panic_on_cancel.to_string()]).unwrap();
    let prepared = runtime.prepare(source, topic.borrowed()).unwrap();
    server
        .connect_transaction(TopicId(id as i32), prepared.id())
        .unwrap()
        .commit()
        .unwrap();
    prepared.commit();
}

#[test]
fn miri_termination_abandonment_cancels_all_before_disconnect_and_waiter_return() {
    for cancel_requested in [false, true] {
        let observation = Observation::new(2);
        let (runtime, source) = observed_runtime(&observation);
        let server = runtime.register_test_server(1);
        seed(&runtime, &server, 1, &source, false);
        seed(&runtime, &server, 2, &source, false);
        let TerminationAdmission::Owner(owner) = server.test_server().begin_termination(&runtime)
        else {
            panic!("first caller must own termination");
        };
        let TerminationAdmission::Waiter(waiter) = server.test_server().begin_termination(&runtime)
        else {
            panic!("second caller must wait for the owner");
        };
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let observation = Arc::clone(&observation);
            let waiting = scope.spawn(move || {
                entered_tx.send(()).unwrap();
                let result = waiter.wait();
                // A failed owner may leave undrained publish operations, but
                // must release the source owners captured by this attempt first.
                observation.assert_complete();
                result
            });
            entered_rx.recv().unwrap();
            if cancel_requested {
                drop(owner.request_cancel());
            } else {
                drop(owner);
            }
            assert!(matches!(waiting.join().unwrap(), Err(XllError::Panic)));
        });
        observation.assert_complete();
        assert_eq!(
            server.test_server().termination_coordinator.phase(),
            TerminationPhase::Failed
        );
        assert!(matches!(server.terminate(), Err(XllError::Panic)));
        assert!(matches!(runtime.close(), Err(XllError::Panic)));
        observation.assert_complete();
    }
}

#[test]
fn runtime_close_notifies_every_server_before_first_disconnect() {
    let observation = Observation::new(2);
    let (runtime, source) = observed_runtime(&observation);
    let first = runtime.register_test_server(1);
    let second = runtime.register_test_server(2);
    seed(&runtime, &first, 1, &source, false);
    seed(&runtime, &second, 2, &source, false);

    runtime.close().unwrap();
    observation.assert_complete();
    assert_eq!(
        first.test_server().termination_coordinator.phase(),
        TerminationPhase::Terminated
    );
    assert_eq!(
        second.test_server().termination_coordinator.phase(),
        TerminationPhase::Terminated
    );
    runtime.close().unwrap();
    observation.assert_complete();
}

#[test]
fn termination_cancel_error_remains_with_its_server_owner() {
    let observation = Observation::new(2);
    let (runtime, source) = observed_runtime(&observation);
    let failing = runtime.register_test_server(1);
    let successful = runtime.register_test_server(2);
    seed(&runtime, &failing, 1, &source, true);
    seed(&runtime, &successful, 2, &source, false);
    let TerminationAdmission::Owner(failing_owner) =
        failing.test_server().begin_termination(&runtime)
    else {
        panic!("first caller must own failing server termination");
    };
    let TerminationAdmission::Owner(successful_owner) =
        successful.test_server().begin_termination(&runtime)
    else {
        panic!("first caller must own successful server termination");
    };

    let failing_owner = failing_owner.request_cancel();
    let successful_owner = successful_owner.request_cancel();
    assert!(matches!(failing_owner.finish(), Err(XllError::Panic)));
    successful_owner.finish().unwrap();
    observation.assert_complete();
    assert!(matches!(failing.terminate(), Err(XllError::Panic)));
    successful.terminate().unwrap();
    assert!(matches!(runtime.close(), Err(XllError::Panic)));
}
