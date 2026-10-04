use std::{
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use xlfn::{
    XllError, XllResult,
    error::InputError,
    rtd::{RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue},
};

// The test publisher runs the same worker and disconnection barriers as the
// real sink, including a synchronous callback that reenters the source.
pub(super) trait Publisher: Send + 'static {
    fn publish(&self, value: RtdValue) -> XllResult<()>;
}

impl Publisher for RtdSink<RtdValue> {
    fn publish(&self, value: RtdValue) -> XllResult<()> {
        RtdSink::publish(self, value)
    }
}

struct RtdPublication<S> {
    sink: Option<S>,
    next_due: Instant,
    sequence: u64,
    last_pulse: u64,
}

struct RtdEntry<S> {
    cancelled: AtomicBool,
    interval: Option<Duration>,
    publication: Mutex<RtdPublication<S>>,
}

impl<S: Publisher> RtdEntry<S> {
    fn publish(&self, pulse: u64, now: Instant, emitted: &AtomicU64) {
        // This mutex is the subscription's terminal barrier. Never hold the
        // source's entries mutex while calling a sink: UpdateNotify may wait
        // for Excel, which can reenter BENCH.RTD.COUNT on another thread.
        let mut publication = self.publication.lock().unwrap_or_else(|p| p.into_inner());
        if self.cancelled.load(Ordering::Acquire) {
            return;
        }
        let value = if pulse > publication.last_pulse {
            publication.last_pulse = pulse;
            pulse
        } else if let Some(interval) = self.interval
            && now >= publication.next_due
        {
            publication.sequence += 1;
            publication.next_due = now + interval;
            publication.sequence
        } else {
            return;
        };
        if let Some(sink) = publication.sink.as_ref() {
            let _ = sink.publish(RtdValue::Number(value as f64));
            emitted.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn drain(&self) {
        // Cancellation rejects later snapshots. Acquiring publication waits
        // for an already admitted sink call, including its COM notification.
        // The framework defers COM disconnection to its cleanup worker, so
        // the Excel callback can return before this barrier finishes.
        self.publication
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sink
            .take();
    }
}

pub(super) struct RtdShared<S: Publisher = RtdSink<RtdValue>> {
    entries: Mutex<HashMap<u64, Arc<RtdEntry<S>>>>,
    wake: Condvar,
    next_id: AtomicU64,
    pulse: AtomicU64,
    periodic_count: AtomicU64,
    fast_count: AtomicU64,
    pub(super) emitted: AtomicU64,
    stop: AtomicBool,
}

impl<S: Publisher> RtdShared<S> {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            next_id: AtomicU64::new(1),
            pulse: AtomicU64::new(0),
            periodic_count: AtomicU64::new(0),
            fast_count: AtomicU64::new(0),
            emitted: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    pub(super) fn count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub(super) fn set_pulse(&self, sequence: u64) {
        // The worker checks its wait predicate while holding entries. Taking
        // the same mutex keeps a pulse from being lost between check and wait.
        let _entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.pulse.store(sequence, Ordering::Release);
        self.wake.notify_one();
    }

    fn insert(&self, sink: S, interval: Option<Duration>) -> (u64, Arc<RtdEntry<S>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entry = Arc::new(RtdEntry {
            cancelled: AtomicBool::new(false),
            interval,
            publication: Mutex::new(RtdPublication {
                sink: Some(sink),
                next_due: Instant::now() + interval.unwrap_or_default(),
                sequence: 0,
                last_pulse: self.pulse.load(Ordering::Acquire),
            }),
        });
        if interval.is_some() {
            self.periodic_count.fetch_add(1, Ordering::AcqRel);
        }
        if interval.is_some_and(|interval| interval < Duration::from_millis(1)) {
            self.fast_count.fetch_add(1, Ordering::AcqRel);
        }
        entries.insert(id, entry.clone());
        drop(entries);
        self.wake.notify_one();
        (id, entry)
    }

    fn disconnect(&self, id: u64, entry: &RtdEntry<S>) {
        entry.cancel();
        {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(removed) = entries.remove(&id) {
                if removed.interval.is_some() {
                    self.periodic_count.fetch_sub(1, Ordering::AcqRel);
                }
                if removed
                    .interval
                    .is_some_and(|interval| interval < Duration::from_millis(1))
                {
                    self.fast_count.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
        self.wake.notify_one();
        entry.drain();
    }

    fn request_stop(&self) {
        let _entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

fn run_worker<S: Publisher>(shared: Arc<RtdShared<S>>) {
    let mut handled_pulse = 0;
    let mut snapshot = Vec::new();
    let mut entries = shared.entries.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        while !shared.stop.load(Ordering::Acquire)
            && shared.pulse.load(Ordering::Acquire) == handled_pulse
            && shared.periodic_count.load(Ordering::Acquire) == 0
        {
            entries = shared.wake.wait(entries).unwrap_or_else(|p| p.into_inner());
        }
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let pulse = shared.pulse.load(Ordering::Acquire);
        let now = Instant::now();
        snapshot.extend(entries.values().cloned());
        drop(entries);
        for entry in snapshot.drain(..) {
            entry.publish(pulse, now, &shared.emitted);
        }
        handled_pulse = pulse;
        entries = shared.entries.lock().unwrap_or_else(|p| p.into_inner());
        if shared.fast_count.load(Ordering::Acquire) > 0 {
            drop(entries);
            std::hint::spin_loop();
            entries = shared.entries.lock().unwrap_or_else(|p| p.into_inner());
        } else if shared.periodic_count.load(Ordering::Acquire) > 0
            && shared.pulse.load(Ordering::Acquire) == handled_pulse
        {
            entries = shared
                .wake
                .wait_timeout(entries, Duration::from_millis(1))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

pub(super) struct BenchRtdSource {
    shared: Arc<RtdShared>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl BenchRtdSource {
    pub(super) fn new() -> (Self, Arc<RtdShared>) {
        let shared = Arc::new(RtdShared::new());
        (
            Self {
                shared: shared.clone(),
                worker: Mutex::new(None),
            },
            shared,
        )
    }

    fn ensure_worker(&self) {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_none() {
            let shared = self.shared.clone();
            *worker = Some(thread::spawn(move || run_worker(shared)));
        }
    }
}

impl Drop for BenchRtdSource {
    fn drop(&mut self) {
        self.shared.request_stop();
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct BenchSubscription {
    shared: Arc<RtdShared>,
    id: u64,
    entry: Arc<RtdEntry<RtdSink<RtdValue>>>,
}

// SAFETY: every sink call holds the entry's publication mutex and checks its
// cancellation flag. Disconnection cancels and removes the entry, then waits
// for that mutex and consumes its only sink before returning. Worker snapshots
// can retain an entry afterward, but cannot use a sink. No map lock is held
// during notification or while waiting for an admitted publication to finish.
unsafe impl RtdSubscription for BenchSubscription {
    fn request_cancel(&self) {
        self.entry.cancel();
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.shared.disconnect(self.id, &self.entry);
        Ok(())
    }
}

// SAFETY: the returned subscription owns the terminal barrier for every sink
// user. A spawn failure occurs before sink transfer into the shared map.
unsafe impl RtdSource for BenchRtdSource {
    type Value = RtdValue;
    type Subscription = BenchSubscription;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<RtdValue>) -> XllResult<BenchSubscription> {
        let period_ms: f64 = topic
            .part(1)
            .ok_or_else(|| XllError::input("period_ms", InputError::OutOfRange))?
            .parse()
            .map_err(|_| XllError::input("period_ms", InputError::OutOfRange))?;
        if !period_ms.is_finite() || period_ms < 0.0 {
            return Err(XllError::input("period_ms", InputError::OutOfRange));
        }
        let interval = (period_ms > 0.0).then(|| Duration::from_secs_f64(period_ms / 1000.0));
        self.ensure_worker();
        let (id, entry) = self.shared.insert(sink, interval);
        Ok(BenchSubscription {
            shared: self.shared.clone(),
            id,
            entry,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const DEADLINE: Duration = Duration::from_secs(5);

    struct MockSink(Box<dyn Fn(RtdValue) + Send>);

    impl Publisher for MockSink {
        fn publish(&self, value: RtdValue) -> XllResult<()> {
            (self.0)(value);
            Ok(())
        }
    }

    #[test]
    fn source_has_no_worker_before_first_subscription() {
        let (source, shared) = BenchRtdSource::new();
        shared.set_pulse(7);
        assert!(source.worker.lock().unwrap().is_none());
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 0);
        drop(source);
        assert!(shared.stop.load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_start_reuses_worker_and_idle_shutdown_wakes_it() {
        let (source, shared) = BenchRtdSource::new();
        let source = Arc::new(source);
        let starters: Vec<_> = (0..4)
            .map(|_| {
                let source = source.clone();
                thread::spawn(move || {
                    source.ensure_worker();
                    source
                        .worker
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .thread()
                        .id()
                })
            })
            .collect();
        let ids: Vec<_> = starters
            .into_iter()
            .map(|starter| starter.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]));
        shared.set_pulse(1);
        let (finished, receiver) = mpsc::channel();
        let closer = thread::spawn(move || {
            drop(source);
            finished.send(()).unwrap();
        });
        receiver
            .recv_timeout(DEADLINE)
            .expect("idle worker must wake and join");
        closer.join().unwrap();
        assert!(shared.stop.load(Ordering::Acquire));
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn notification_reenters_count_and_every_topic_in_snapshot_is_published() {
        let shared = Arc::new(RtdShared::new());
        let (notified, receiver) = mpsc::channel();
        for topic in 0..3 {
            let reentrant = Arc::downgrade(&shared);
            let notified = notified.clone();
            shared.insert(
                MockSink(Box::new(move |value| {
                    let reentrant = reentrant.upgrade().unwrap();
                    assert!(
                        reentrant.entries.try_lock().is_ok(),
                        "notification holds entries"
                    );
                    assert_eq!(reentrant.count(), 3);
                    assert_eq!(value, RtdValue::Number(7.0));
                    notified.send(topic).unwrap();
                })),
                None,
            );
        }
        let worker_shared = shared.clone();
        let worker = thread::spawn(move || run_worker(worker_shared));
        shared.set_pulse(7);
        let mut seen: Vec<_> = (0..3)
            .map(|_| receiver.recv_timeout(DEADLINE).unwrap())
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);
        shared.request_stop();
        worker.join().unwrap();
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn pulse_arriving_during_notification_is_delivered_without_lost_wakeup() {
        let shared = Arc::new(RtdShared::new());
        let (notified, receiver) = mpsc::channel();
        for topic in 0..2 {
            let reentrant = Arc::downgrade(&shared);
            let notified = notified.clone();
            shared.insert(
                MockSink(Box::new(move |value| {
                    let RtdValue::Number(value) = value else {
                        panic!("expected a numeric pulse");
                    };
                    if value == 7.0 {
                        reentrant.upgrade().unwrap().set_pulse(8);
                    }
                    notified.send((topic, value as u64)).unwrap();
                })),
                None,
            );
        }
        let worker_shared = shared.clone();
        let worker = thread::spawn(move || run_worker(worker_shared));
        shared.set_pulse(7);
        let mut seen: Vec<_> = (0..4)
            .map(|_| receiver.recv_timeout(DEADLINE).unwrap())
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![(0, 7), (0, 8), (1, 7), (1, 8)]);
        shared.request_stop();
        worker.join().unwrap();
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn disconnect_waits_for_notification_and_old_snapshot_cannot_reuse_sink() {
        let shared = Arc::new(RtdShared::new());
        let calls = Arc::new(AtomicU64::new(0));
        let observed_calls = calls.clone();
        let (entered, enter_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (id, entry) = shared.insert(
            MockSink(Box::new(move |_| {
                observed_calls.fetch_add(1, Ordering::Relaxed);
                entered.send(()).unwrap();
                release_rx.recv_timeout(DEADLINE).unwrap();
            })),
            None,
        );
        let publishing_entry = entry.clone();
        let publishing_shared = shared.clone();
        let publishing = thread::spawn(move || {
            publishing_entry.publish(1, Instant::now(), &publishing_shared.emitted)
        });
        enter_rx.recv_timeout(DEADLINE).unwrap();
        entry.cancel();
        let closing_shared = shared.clone();
        let closing_entry = entry.clone();
        let (completed, complete_rx) = mpsc::channel();
        let closing = thread::spawn(move || {
            closing_shared.disconnect(id, &closing_entry);
            completed.send(()).unwrap();
        });
        // Removal finishes while the notifier is blocked. Its map lock is
        // available to an Excel-style reentrant count, but cleanup must wait.
        let deadline = Instant::now() + DEADLINE;
        while shared.count() != 0 {
            assert!(Instant::now() < deadline, "disconnect must release entries");
            thread::yield_now();
        }
        assert!(complete_rx.try_recv().is_err());
        release.send(()).unwrap();
        complete_rx.recv_timeout(DEADLINE).unwrap();
        closing.join().unwrap();
        publishing.join().unwrap();
        entry.publish(2, Instant::now(), &shared.emitted);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(entry.publication.lock().unwrap().sink.is_none());
    }

    #[test]
    fn disconnect_without_prior_cancel_also_closes_a_retained_snapshot() {
        let shared = RtdShared::new();
        let (id, entry) = shared.insert(
            MockSink(Box::new(|_| panic!("disconnected sink used"))),
            Some(Duration::from_micros(10)),
        );
        shared.disconnect(id, &entry);
        entry.publish(1, Instant::now(), &shared.emitted);
        assert_eq!(shared.count(), 0);
        assert_eq!(shared.periodic_count.load(Ordering::Relaxed), 0);
        assert_eq!(shared.fast_count.load(Ordering::Relaxed), 0);
    }
}
