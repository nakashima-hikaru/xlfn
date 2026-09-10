//! Benchmark-only shared publisher experiment. No production topology changes.
#![allow(
    unsafe_code,
    reason = "Benchmark source owns topic publication barriers and joins its workers"
)]
use super::{
    IntoRtdValue, RefreshOutcome, RtdChannelSource, RtdSink, RtdSource, RtdSubscription, RtdTopic,
    SourceRegistration, StoredRtdValue, SubscriptionRuntime, TopicId,
};
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{Builder, JoinHandle},
    time::{Duration, Instant},
};

type Job = (i32, usize);
type Receivers = Arc<Mutex<Vec<Option<mpsc::Receiver<Job>>>>>;

struct ShardState {
    ready: VecDeque<Arc<SharedTopic>>,
    stopped: bool,
}
struct SharedShard {
    state: Mutex<ShardState>,
    changed: Condvar,
}
impl SharedShard {
    fn schedule(&self, topic: Arc<SharedTopic>) {
        let mut state = self.state.lock();
        if state.stopped {
            return;
        }
        state.ready.push_back(topic);
        drop(state);
        self.changed.notify_one();
    }
    fn run(&self) {
        loop {
            let topic = {
                let mut state = self.state.lock();
                while state.ready.is_empty() && !state.stopped {
                    self.changed.wait(&mut state);
                }
                if state.stopped {
                    return;
                }
                state.ready.pop_front().unwrap()
            };
            topic.publish_batch();
        }
    }
}
struct SharedPool {
    shards: Vec<Arc<SharedShard>>,
    workers: Vec<JoinHandle<()>>,
}
impl SharedPool {
    fn new(count: usize) -> Self {
        let mut pool = Self {
            shards: Vec::new(),
            workers: Vec::new(),
        };
        for _ in 0..count {
            let shard = Arc::new(SharedShard {
                state: Mutex::new(ShardState {
                    ready: VecDeque::new(),
                    stopped: false,
                }),
                changed: Condvar::new(),
            });
            let worker_shard = Arc::clone(&shard);
            pool.shards.push(shard);
            pool.workers.push(
                Builder::new()
                    .name("rtd-shared-probe".into())
                    .spawn(move || worker_shard.run())
                    .unwrap(),
            );
        }
        pool
    }
}
impl Drop for SharedPool {
    fn drop(&mut self) {
        for shard in &self.shards {
            let pending = {
                let mut state = shard.state.lock();
                state.stopped = true;
                std::mem::take(&mut state.ready)
            };
            drop(pending);
            shard.changed.notify_all();
        }
        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}
struct TopicState {
    values: VecDeque<StoredRtdValue>,
    sink: Option<RtdSink<i32>>,
    scheduled: bool,
    publishing: bool,
    error: Option<XllError>,
}
struct SharedTopic {
    state: Mutex<TopicState>,
    idle: Condvar,
    stopped: AtomicBool,
    shard: Arc<SharedShard>,
}
impl SharedTopic {
    fn send(self: &Arc<Self>, value: i32) -> XllResult<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        let value = value.into_rtd_value()?.into_stored()?;
        let schedule = {
            let mut state = self.state.lock();
            if self.stopped.load(Ordering::Acquire) {
                return Err(XllError::Closing);
            }
            if state.values.len() == 64 {
                return Err(XllError::Overloaded);
            }
            state.values.push_back(value);
            let schedule = !state.scheduled;
            state.scheduled = true;
            schedule
        };
        if schedule {
            self.shard.schedule(Arc::clone(self));
        }
        Ok(())
    }
    fn publish_batch(self: &Arc<Self>) {
        let (sink, mut batch) = {
            let mut state = self.state.lock();
            if self.stopped.load(Ordering::Acquire) {
                return;
            }
            state.publishing = true;
            let mut batch = smallvec::SmallVec::<[StoredRtdValue; 32]>::new();
            for _ in 0..32 {
                let Some(value) = state.values.pop_front() else {
                    break;
                };
                batch.push(value);
            }
            (state.sink.as_ref().unwrap().clone(), batch)
        };
        let mut error = None;
        for value in batch.drain(..) {
            if self.stopped.load(Ordering::Acquire) {
                break;
            }
            if let Err(failure) = sink.publish_stored(value) {
                error = Some(failure);
                self.stopped.store(true, Ordering::Release);
                break;
            }
        }
        // Drop the final local sink before signalling the disconnect barrier.
        #[allow(
            clippy::drop_non_drop,
            reason = "Consume the non-owning sink capability before the disconnect barrier is released"
        )]
        drop(sink);
        let schedule = {
            let mut state = self.state.lock();
            state.publishing = false;
            state.error = error;
            let schedule = !state.values.is_empty() && !self.stopped.load(Ordering::Acquire);
            state.scheduled = schedule;
            self.idle.notify_all();
            schedule
        };
        if schedule {
            self.shard.schedule(Arc::clone(self));
        }
    }
    fn close(&self) {
        let mut state = self.state.lock();
        self.stopped.store(true, Ordering::Release);
        state.values.clear();
    }
    fn finish(&self) -> XllResult<()> {
        self.close();
        let mut state = self.state.lock();
        while state.publishing {
            self.idle.wait(&mut state);
        }
        state.sink.take();
        state.error.take().map_or(Ok(()), Err)
    }
}
struct SharedSubscription {
    topic: Arc<SharedTopic>,
    producer: Option<JoinHandle<XllResult<()>>>,
}
impl SharedSubscription {
    fn finish(&mut self) -> XllResult<()> {
        let publication = self.topic.finish();
        let production = self
            .producer
            .take()
            .map(|worker| worker.join().unwrap())
            .unwrap_or(Ok(()));
        publication.and(production)
    }
}
impl Drop for SharedSubscription {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
// SAFETY: closing disables publication, waits for the sole publisher to drop
// its sink clone, clears the stored sink, then joins the producer. Ready-queue
// references surviving disconnect can only observe a closed topic with no sink.
unsafe impl RtdSubscription for SharedSubscription {
    fn request_cancel(&self) {
        self.topic.close();
    }
    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.finish()
    }
}
struct SharedSource {
    pool: Arc<SharedPool>,
    receivers: Receivers,
    done: mpsc::Sender<usize>,
}
// SAFETY: every subscription owns a disconnect barrier before it is returned.
// The pool only accesses sinks while that topic's publication barrier is held.
unsafe impl RtdSource for SharedSource {
    type Value = i32;
    type Subscription = SharedSubscription;
    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<i32>) -> XllResult<Self::Subscription> {
        let index: usize = topic.parts()[0].parse().unwrap();
        let topic = Arc::new(SharedTopic {
            state: Mutex::new(TopicState {
                values: VecDeque::new(),
                sink: Some(sink),
                scheduled: false,
                publishing: false,
                error: None,
            }),
            idle: Condvar::new(),
            stopped: AtomicBool::new(false),
            shard: Arc::clone(&self.pool.shards[index % self.pool.shards.len()]),
        });
        let mut subscription = SharedSubscription {
            topic: Arc::clone(&topic),
            producer: None,
        };
        let receiver = self.receivers.lock()[index].take().unwrap();
        let done = self.done.clone();
        subscription.producer = Some(
            Builder::new()
                .name("rtd-shared-producer-probe".into())
                .spawn(move || run_producer(receiver, done, |value| topic.send(value)))
                .map_err(|error| XllError::Native {
                    code: error.raw_os_error().unwrap_or(0),
                    message: error.to_string(),
                })?,
        );
        Ok(subscription)
    }
}
fn run_producer(
    receiver: mpsc::Receiver<Job>,
    done: mpsc::Sender<usize>,
    send: impl Fn(i32) -> XllResult<()>,
) -> XllResult<()> {
    while let Ok((revision, updates)) = receiver.recv() {
        let mut retries = 0;
        for index in 0..updates {
            let value = revision * 1_000_000 + index as i32;
            loop {
                match send(value) {
                    Ok(()) => break,
                    Err(XllError::Overloaded) => {
                        retries += 1;
                        std::thread::yield_now();
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        done.send(retries).unwrap();
    }
    Ok(())
}

fn measure_source<S: RtdSource>(
    source: S,
    registration: SourceRegistration,
    jobs: Vec<mpsc::SyncSender<Job>>,
    done: mpsc::Receiver<usize>,
    subscriptions: usize,
    updates: usize,
) -> serde_json::Value {
    let started = Instant::now();
    let source = registration.register(source).unwrap();
    let runtime = Box::new(SubscriptionRuntime::with_sources_for_internal(
        registration.finish(),
    ));
    // Drop command senders before runtime teardown, including failed setup.
    // Every waiting producer exits when its receiver disconnects.
    let jobs = scopeguard::guard(jobs, drop);
    // SAFETY: boxed runtime remains stable until all source subscriptions stop.
    let server = unsafe {
        runtime
            .register_server(super::ServerGeneration::new(1).unwrap())
            .unwrap()
    };
    for index in 0..subscriptions {
        let prepared = runtime
            .prepare(&source, RtdTopic::single(index.to_string()).unwrap())
            .unwrap();
        runtime
            .connect_transaction(&server, TopicId(index as i32 + 1), prepared.id())
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
    }
    let setup_ns = started.elapsed().as_nanos();
    let mut rounds = Vec::new();
    let mut retries = 0;
    let mut previous = vec![None; subscriptions];
    for revision in 1..=7 {
        let started = Instant::now();
        for job in jobs.iter() {
            job.send((revision, updates)).unwrap();
        }
        for _ in 0..subscriptions {
            retries += done.recv_timeout(Duration::from_secs(60)).unwrap();
        }
        let last_value = revision * 1_000_000 + updates as i32 - 1;
        let mut delivered = vec![false; subscriptions];
        let mut remaining = subscriptions;
        let deadline = Instant::now() + Duration::from_secs(60);
        while remaining > 0 {
            let batch = server.begin_refresh().unwrap();
            for update in &batch.updates {
                let index = update.topic_id as usize - 1;
                if update.value == StoredRtdValue::Integer(last_value) && !delivered[index] {
                    if let Some(previous) = previous[index] {
                        assert_eq!(update.sequence - previous, updates as u64);
                    }
                    previous[index] = Some(update.sequence);
                    delivered[index] = true;
                    remaining -= 1;
                }
            }
            batch.complete(RefreshOutcome::Delivered).unwrap();
            assert!(
                Instant::now() < deadline,
                "all subscriptions must deliver their final value"
            );
            if remaining > 0 {
                std::thread::yield_now();
            }
        }
        if revision > 1 {
            rounds.push(started.elapsed().as_nanos() as u64);
        }
    }
    let started = Instant::now();
    drop(jobs);
    drop(runtime);
    let teardown_ns = started.elapsed().as_nanos();
    rounds.sort_unstable();
    serde_json::json!({ "subscriptions": subscriptions, "updates_per_subscription": updates,
        "rounds_ns": rounds, "median_ns": rounds[rounds.len()/2], "overloaded_retries": retries,
        "subscription_setup_ns_excluding_pool": setup_ns, "teardown_ns": teardown_ns })
}

pub fn shared_publisher_topology_probe(
    subscriptions: usize,
    shared_publishers: usize,
    updates: usize,
) -> serde_json::Value {
    let mut jobs = Vec::new();
    let mut receivers = Vec::new();
    for _ in 0..subscriptions {
        let (sender, receiver) = mpsc::sync_channel(1);
        jobs.push(sender);
        receivers.push(Some(receiver));
    }
    let receivers = Arc::new(Mutex::new(receivers));
    let (done_tx, done_rx) = mpsc::channel();
    let registration =
        SourceRegistration::new(crate::generation::RuntimeGeneration::new(1).unwrap());
    let mut result = if shared_publishers == 0 {
        let source = RtdChannelSource::new(NonZeroUsize::new(64).unwrap(), move |topic| {
            let index: usize = topic.parts()[0].parse().unwrap();
            let receiver = receivers.lock()[index].take().unwrap();
            let done_tx = done_tx.clone();
            Ok(move |sender: super::RtdSender<i32>| {
                run_producer(receiver, done_tx, |value| sender.try_send(value))
            })
        });
        measure_source(source, registration, jobs, done_rx, subscriptions, updates)
    } else {
        let pool = Arc::new(SharedPool::new(shared_publishers));
        let source = SharedSource {
            pool,
            receivers,
            done: done_tx,
        };
        measure_source(source, registration, jobs, done_rx, subscriptions, updates)
    };
    result["shared_publishers"] = shared_publishers.into();
    result["owned_worker_threads"] = (subscriptions
        + if shared_publishers == 0 {
            subscriptions
        } else {
            shared_publishers
        })
    .into();
    result
}
