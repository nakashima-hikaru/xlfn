#![allow(
    unsafe_code,
    reason = "The channel adapter owns and joins the only worker allowed to use an RTD sink"
)]

use super::source::{RtdSink, RtdSource, RtdSubscription};
use super::topic::RtdTopic;
use super::value::{IntoRtdValue, StoredRtdValue};
use crate::sync::{Condvar, Mutex};
use crate::{ExcelError, XllError, XllResult};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{Builder, JoinHandle};
use std::time::Duration;
use triomphe::Arc;

type Producer<T> = dyn FnOnce(RtdSender<T>) -> XllResult<()> + Send;
type ProducerFactory<T> = dyn Fn(RtdTopic) -> XllResult<Box<Producer<T>>> + Send + Sync;

/// Cell behavior when an RTD producer returns an error or panics.
///
/// Both policies report the failure to the diagnostic sink immediately.
/// Successful completion and cancellation do not publish an error value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum RtdProducerErrorPolicy {
    /// Drain accepted updates and retain the last value (the default).
    #[default]
    RetainLastValue,
    /// Drain accepted updates, then publish this Excel error as the final value.
    PublishError(ExcelError),
}

struct ProducerSlots {
    state: Mutex<ProducerSlotState>,
}

struct ProducerSlotState {
    limit: NonZeroUsize,
    used: usize,
}

impl ProducerSlots {
    fn reserve(this: &Arc<Self>) -> XllResult<ProducerPermit> {
        let mut state = this.state.lock();
        if state.used >= state.limit.get() {
            return Err(XllError::Overloaded);
        }
        state.used += 1;
        Ok(ProducerPermit(Arc::clone(this)))
    }
}

// Retained until join, including for a producer that has already exited.
struct ProducerPermit(Arc<ProducerSlots>);

impl Drop for ProducerPermit {
    fn drop(&mut self) {
        self.0.state.lock().used -= 1;
    }
}

/// A safe RTD source with a bounded queue and framework-owned workers.
///
/// The source uniquely owns a factory. For each topic the factory transfers a
/// new producer job to its subscription's worker. That job owns its captures
/// and sees only an [`RtdSender`]. One generation-owned shared publisher uses
/// the non-owning [`RtdSink`] and processes at most 32 values per topic turn.
/// Source destruction never controls a running job's lifetime. Disconnect
/// closes the queue, waits for in-flight publication, revokes the sink, and
/// joins the producer, including when the producer fails or panics. Sender
/// clones may outlive the subscription: they retain only a closed queue and
/// cannot access the RTD runtime.
///
/// The producer must return when its sender closes. Use bounded I/O and
/// [`RtdSender::wait_closed`] for cancellation-aware polling. A producer that
/// never returns delays disconnect and add-in unload. Join any additional
/// threads or callbacks before returning from the producer.
pub struct RtdChannelSource<T> {
    capacity: NonZeroUsize,
    factory: Box<ProducerFactory<T>>,
    producers: Arc<ProducerSlots>,
    error_policy: RtdProducerErrorPolicy,
}

impl<T: IntoRtdValue + Send + 'static> RtdChannelSource<T> {
    /// Creates a source with a per-subscription queue capacity.
    ///
    /// `factory` runs synchronously during subscription setup and returns an
    /// owned job, or fails before any worker starts. Each job runs once on its
    /// own worker and may own resources that are neither `Clone` nor `Sync`.
    /// Its error or panic closes sender admission and is diagnosed immediately;
    /// [`Self::with_error_policy`] selects the final cell value. A successful
    /// return closes sender admission and drains
    /// accepted values. The shared publisher remains available to other topics.
    /// Job resources are dropped before the producer worker is joined.
    pub fn new<P>(
        capacity: NonZeroUsize,
        factory: impl Fn(RtdTopic) -> XllResult<P> + Send + Sync + 'static,
    ) -> Self
    where
        P: FnOnce(RtdSender<T>) -> XllResult<()> + Send + 'static,
    {
        Self {
            capacity,
            factory: Box::new(move |topic| {
                factory(topic).map(|job| Box::new(job) as Box<Producer<T>>)
            }),
            producers: Arc::new(ProducerSlots {
                state: Mutex::new(ProducerSlotState {
                    limit: NonZeroUsize::new(64).expect("default producer limit is nonzero"),
                    used: 0,
                }),
            }),
            error_policy: RtdProducerErrorPolicy::default(),
        }
    }

    /// Limits producer threads owned by this source, including exited workers
    /// that have not been joined. The default is 64. At capacity, subscription
    /// setup returns [`XllError::Overloaded`] before invoking the factory.
    ///
    /// Each subscription retains its slot until disconnect joins its worker.
    /// Changing this limit preserves existing reservations; lowering it below
    /// the retained worker count rejects setup until enough workers are joined.
    /// This adapter is intended for tens of independent blocking producers;
    /// thousands of topics should use a custom multiplexed source instead.
    /// All sources in a generation additionally share one publisher thread.
    #[must_use]
    pub fn with_max_producers(self, limit: NonZeroUsize) -> Self {
        self.producers.state.lock().limit = limit;
        self
    }

    /// Selects whether a producer failure retains its final accepted value or
    /// replaces it with an Excel error. Failure publication uses a reserved
    /// terminal slot, so a full data queue cannot hide the error.
    #[must_use]
    pub fn with_error_policy(mut self, policy: RtdProducerErrorPolicy) -> Self {
        self.error_policy = policy;
        self
    }

    /// Returns the configured maximum number of retained producer workers.
    #[must_use]
    pub fn max_producers(&self) -> NonZeroUsize {
        self.producers.state.lock().limit
    }

    /// Returns the configured behavior after a producer failure.
    #[must_use]
    pub const fn error_policy(&self) -> RtdProducerErrorPolicy {
        self.error_policy
    }
}

/// A cloneable queue sender that cannot use an RTD publication capability.
///
/// Values are converted and validated on the calling thread, before the queue
/// is locked. Successful enqueueing does not guarantee delivery: disconnect
/// discards pending values, and an RTD publication error closes the channel.
///
/// The subscription exclusively owns shutdown and worker joins. Reference
/// counting only shares queue storage among senders and those workers; it
/// cannot extend admission or retain queued payloads after disconnect. A
/// sender that survives disconnect retains an empty, closed queue allocation.
pub struct RtdSender<T> {
    channel: Arc<Channel>,
    _value: PhantomData<fn(T)>,
}

impl<T> Clone for RtdSender<T> {
    fn clone(&self) -> Self {
        Self {
            channel: Arc::clone(&self.channel),
            _value: PhantomData,
        }
    }
}

impl<T> RtdSender<T> {
    /// Returns whether this subscription has stopped accepting values.
    pub fn is_closed(&self) -> bool {
        !self.channel.accepting.load(Ordering::Acquire)
    }

    /// Waits until admission closes or `timeout` elapses. Returns `true` when
    /// closed, so a polling producer can exit promptly during disconnect.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let mut state = self.channel.state.lock();
        self.channel
            .closed
            .wait_while_for(&mut state, |state| state.accepting, timeout);
        !state.accepting
    }
}

/// Validated value retained when channel admission fails. It can be retried
/// without cloning its input or repeating custom conversion.
/// It can be sent to any sender with the same input type `T`, including another
/// subscription. It owns the converted RTD value, not the original `T`; the type
/// marker adds no `Clone`, `Debug`, `Send`, or `Sync` requirement on `T`.
pub struct RtdPendingValue<T> {
    value: StoredRtdValue,
    // Invariant in T; conversion has consumed T, so it imposes no auto-trait bounds.
    _input: PhantomData<fn(T) -> T>,
}
impl<T> std::fmt::Debug for RtdPendingValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RtdPendingValue").field(&self.value).finish()
    }
}

/// Failure to convert or admit an update. Full/Closed preserve its payload.
#[non_exhaustive]
pub enum RtdSendError<T> {
    /// Application conversion or candidate validation failed.
    Invalid(XllError),
    /// The bounded queue is full; the validated payload can be retried.
    Full(RtdPendingValue<T>),
    /// Admission has closed; the validated payload is retained for recovery.
    Closed(RtdPendingValue<T>),
}
impl<T> RtdSendError<T> {
    /// Recovers a validated payload; invalid conversion has no recoverable value.
    pub fn into_pending(self) -> Result<RtdPendingValue<T>, XllError> {
        match self {
            Self::Invalid(error) => Err(error),
            Self::Full(value) | Self::Closed(value) => Ok(value),
        }
    }
}
impl<T> std::fmt::Display for RtdSendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Full(_) => f.write_str("RTD queue is full"),
            Self::Closed(_) => f.write_str("RTD channel is closed"),
        }
    }
}
impl<T> std::error::Error for RtdSendError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(error) => Some(error),
            Self::Full(_) | Self::Closed(_) => None,
        }
    }
}
impl<T> std::fmt::Debug for RtdSendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => f.debug_tuple("Invalid").field(error).finish(),
            Self::Full(value) => f.debug_tuple("Full").field(value).finish(),
            Self::Closed(value) => f.debug_tuple("Closed").field(value).finish(),
        }
    }
}
impl<T> RtdSendError<T> {
    /// Discards a retained update when propagation intentionally stops its producer.
    pub fn into_error(self) -> XllError {
        match self {
            RtdSendError::Invalid(error) => error,
            RtdSendError::Full(_) => XllError::Overloaded,
            RtdSendError::Closed(_) => XllError::Closing,
        }
    }
}

impl<T: IntoRtdValue> RtdSender<T> {
    /// Converts once, validates, then tries bounded admission. Full/Closed
    /// return the converted value for `try_send_pending`; conversion runs even
    /// on a closed channel so a valid rejected payload remains recoverable.
    pub fn try_send(&self, value: T) -> Result<(), RtdSendError<T>> {
        let value = value
            .into_rtd_value()
            .and_then(|value| value.into_stored())
            .map_err(RtdSendError::Invalid)?;
        self.try_send_pending(RtdPendingValue {
            value,
            _input: PhantomData,
        })
    }
}
impl<T> RtdSender<T> {
    /// Retries an already validated value without re-running its converter.
    /// The pending value must have the same input type as this sender; it need
    /// not originate from this subscription. Full/Closed return it again.
    pub fn try_send_pending(&self, value: RtdPendingValue<T>) -> Result<(), RtdSendError<T>> {
        let wake_publisher = {
            let mut state = self.channel.state.lock();
            if !state.accepting {
                return Err(RtdSendError::Closed(value));
            }
            if state.values.len() >= self.channel.capacity.get() {
                return Err(RtdSendError::Full(value));
            }
            let was_empty = state.values.is_empty();
            state.values.push_back(value.value);
            was_empty
        };
        if wake_publisher {
            #[cfg(any(test, feature = "bench-internals"))]
            self.channel.changed.notify_one();
            Channel::schedule(&self.channel);
        }
        Ok(())
    }
}

struct Publication {
    sink: Option<super::ErasedSink>,
    result: XllResult<()>,
}

struct Ready {
    channels: VecDeque<Arc<Channel>>,
    stopping: bool,
}

pub(super) struct PublisherQueue {
    ready: Mutex<Ready>,
    changed: Condvar,
}

impl PublisherQueue {
    fn push(&self, channel: Arc<Channel>) {
        let mut ready = self.ready.lock();
        if !ready.stopping {
            ready.channels.push_back(channel);
            self.changed.notify_one();
        }
    }

    fn run(&self) {
        loop {
            let channel = {
                let mut ready = self.ready.lock();
                while ready.channels.is_empty() && !ready.stopping {
                    self.changed.wait(&mut ready);
                }
                if ready.stopping {
                    return;
                }
                ready.channels.pop_front().expect("ready queue is nonempty")
            };
            Channel::publish_batch(&channel);
        }
    }
}

/// Generation-owned, lazily started publisher. One ready-queue entry per
/// topic; at most 32 FIFO values per turn gives other ready topics progress.
/// Surviving senders may retain the inert queue, never a worker or a sink.
enum PublisherWorker {
    Dormant,
    Running(Arc<PublisherQueue>, JoinHandle<()>),
    Stopped,
}

pub(super) struct PublisherPool {
    worker: Mutex<PublisherWorker>,
}

impl PublisherPool {
    pub(super) const fn new() -> Self {
        Self {
            worker: Mutex::new(PublisherWorker::Dormant),
        }
    }

    pub(super) fn queue(&self) -> XllResult<Arc<PublisherQueue>> {
        let mut worker = self.worker.lock();
        match &*worker {
            PublisherWorker::Running(queue, _) => return Ok(Arc::clone(queue)),
            PublisherWorker::Stopped => return Err(XllError::Closing),
            PublisherWorker::Dormant => {}
        }
        let queue = Arc::new(PublisherQueue {
            ready: Mutex::new(Ready {
                channels: VecDeque::new(),
                stopping: false,
            }),
            changed: Condvar::new(),
        });
        let shared = Arc::clone(&queue);
        let handle = Builder::new()
            .name("xlfn-rtd-publisher".into())
            .spawn(move || shared.run())
            .map_err(spawn_error)?;
        *worker = PublisherWorker::Running(Arc::clone(&queue), handle);
        Ok(queue)
    }

    pub(super) fn stop(&self) {
        let mut worker = self.worker.lock();
        if let PublisherWorker::Running(queue, handle) =
            std::mem::replace(&mut *worker, PublisherWorker::Stopped)
        {
            let pending = {
                let mut ready = queue.ready.lock();
                ready.stopping = true;
                std::mem::take(&mut ready.channels)
            };
            queue.changed.notify_all();
            let _ = crate::panic_boundary::contain_panic(handle.join());
            drop(pending);
        }
    }
}

impl Drop for PublisherPool {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ChannelState {
    values: VecDeque<StoredRtdValue>,
    terminal_error: Option<ExcelError>,
    accepting: bool,
    stopping: bool,
    scheduled: bool,
}

struct Channel {
    capacity: NonZeroUsize,
    publication: Mutex<Publication>,
    #[cfg(test)]
    panic_publish: AtomicBool,
    scheduler: Mutex<Option<Arc<PublisherQueue>>>,
    state: Mutex<ChannelState>,
    #[cfg(any(test, feature = "bench-internals"))]
    changed: Condvar,
    // Cancellation waiters must never consume a publisher wakeup.
    closed: Condvar,
    accepting: AtomicBool,
    stopping: AtomicBool,
}

impl Channel {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            #[cfg(test)]
            panic_publish: AtomicBool::new(false),
            publication: Mutex::new(Publication {
                sink: None,
                result: Ok(()),
            }),
            scheduler: Mutex::new(None),
            state: Mutex::new(ChannelState {
                values: VecDeque::new(),
                terminal_error: None,
                accepting: true,
                stopping: false,
                scheduled: false,
            }),
            #[cfg(any(test, feature = "bench-internals"))]
            changed: Condvar::new(),
            closed: Condvar::new(),
            accepting: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
        }
    }

    fn sender<T>(this: &Arc<Self>) -> RtdSender<T> {
        RtdSender {
            channel: Arc::clone(this),
            _value: PhantomData,
        }
    }

    fn schedule(this: &Arc<Self>) {
        let scheduler = this.scheduler.lock();
        let Some(queue) = scheduler.as_ref() else {
            return;
        };
        let mut state = this.state.lock();
        if state.stopping
            || state.scheduled
            || (state.values.is_empty() && state.terminal_error.is_none())
        {
            return;
        }
        state.scheduled = true;
        queue.push(Arc::clone(this));
    }

    fn publish_batch(this: &Arc<Self>) {
        let mut publication = this.publication.lock();
        if let Some(sink) = publication.sink {
            // Catch within the publication lock: disconnect observes the error
            // and the worker remains available to unrelated subscriptions.
            let result =
                crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                    #[cfg(test)]
                    assert!(
                        !this.panic_publish.swap(false, Ordering::Relaxed),
                        "injected publisher panic"
                    );
                    let mut batch = super::delivery::PublishBatch::new();
                    {
                        let mut state = this.state.lock();
                        if !state.stopping {
                            for _ in 0..32 {
                                let Some(value) = state.values.pop_front().or_else(|| {
                                    state.terminal_error.take().map(StoredRtdValue::Error)
                                }) else {
                                    break;
                                };
                                batch.push(value);
                            }
                        }
                    }
                    sink.publish_batch(batch, &this.stopping)
                }))
                .unwrap_or(Err(XllError::Panic));
            if result.is_err() {
                publication.result = result;
                publication.sink = None;
                this.close();
            }
        }
        drop(publication);
        {
            let mut state = this.state.lock();
            state.scheduled = false;
            #[cfg(any(test, feature = "bench-internals"))]
            this.changed.notify_all();
        }
        Self::schedule(this);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn producer_finished(&self) {
        {
            let mut state = self.state.lock();
            state.accepting = false;
            self.accepting.store(false, Ordering::Release);
        }
        self.changed.notify_all();
        self.closed.notify_all();
    }

    fn producer_completed(
        this: &Arc<Self>,
        topic: &RtdTopic,
        result: XllResult<()>,
        policy: RtdProducerErrorPolicy,
    ) {
        let mut failure = result.err();
        {
            let mut state = this.state.lock();
            if state.stopping && matches!(failure, Some(XllError::Closing)) {
                failure = None;
            }
            state.accepting = false;
            this.accepting.store(false, Ordering::Release);
            if failure.is_some()
                && !state.stopping
                && let RtdProducerErrorPolicy::PublishError(error) = policy
            {
                state.terminal_error = Some(error);
            }
        }
        #[cfg(any(test, feature = "bench-internals"))]
        this.changed.notify_all();
        this.closed.notify_all();
        Self::schedule(this);
        if let Some(error) = failure.as_ref() {
            crate::diagnostics::report_no_unwind(
                "RTD producer",
                &XllError::RtdProducerFailure {
                    topic: format!("{:?}", topic.parts().collect::<Vec<_>>()),
                    source: Box::new(error.clone()),
                },
            );
        }
    }

    fn close(&self) {
        let pending = {
            let mut state = self.state.lock();
            state.accepting = false;
            self.accepting.store(false, Ordering::Release);
            state.stopping = true;
            self.stopping.store(true, Ordering::Release);
            state.terminal_error = None;
            std::mem::take(&mut state.values)
        };
        #[cfg(any(test, feature = "bench-internals"))]
        self.changed.notify_all();
        self.closed.notify_all();
        drop(pending);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    fn receive_batch(&self, batch: &mut smallvec::SmallVec<[StoredRtdValue; 32]>) -> bool {
        debug_assert!(batch.is_empty());
        let mut state = self.state.lock();
        loop {
            if state.stopping {
                return false;
            }
            if !state.values.is_empty() {
                for _ in 0..32 {
                    let Some(value) = state.values.pop_front() else {
                        break;
                    };
                    batch.push(value);
                }
                return true;
            }
            if !state.accepting {
                return false;
            }
            self.changed.wait(&mut state);
        }
    }

    #[cfg(test)]
    fn receive(&self) -> Option<StoredRtdValue> {
        let mut state = self.state.lock();
        loop {
            if state.stopping {
                return None;
            }
            if let Some(value) = state.values.pop_front() {
                return Some(value);
            }
            if !state.accepting {
                return None;
            }
            self.changed.wait(&mut state);
        }
    }
}

/// Framework-owned shutdown barrier returned by [`RtdChannelSource`].
///
/// Its private handles can only be created by the adapter. Dropping this value
/// also closes admission, joins the producer, and waits for publication.
/// Failed setup cannot leave a usable sink behind.
/// Producer failures are diagnosed at completion and do not become duplicate
/// disconnect failures. Disconnect reports publication and worker-join errors.
pub struct RtdChannelSubscription {
    channel: Arc<Channel>,
    producer: Option<JoinHandle<()>>,
    producer_permit: Option<ProducerPermit>,
}

impl RtdChannelSubscription {
    fn start_publisher<T: IntoRtdValue + Send + 'static>(
        channel: Arc<Channel>,
        sink: RtdSink<T>,
    ) -> XllResult<Self> {
        let queue = sink.publisher_queue()?;
        channel.publication.lock().sink = Some(sink.erased());
        *channel.scheduler.lock() = Some(queue);
        Channel::schedule(&channel);
        Ok(Self {
            channel,
            producer: None,
            producer_permit: None,
        })
    }

    fn finish(&mut self) -> XllResult<()> {
        self.channel.close();
        // Complete both barriers before reporting errors. Consume producer
        // panic payloads without running arbitrary destructors in Drop.
        // Locking publication waits for the current batch, then revokes the
        // only sink. Queued work can survive this barrier but cannot publish.
        let publisher = {
            let mut publication = self.channel.publication.lock();
            publication.sink.take();
            Some(Ok(std::mem::replace(&mut publication.result, Ok(()))))
        };
        self.channel.scheduler.lock().take();
        let producer = self
            .producer
            .take()
            .map(|worker| crate::panic_boundary::contain_panic(worker.join()).map(|()| Ok(())));
        self.producer_permit.take();
        for result in [publisher, producer].into_iter().flatten() {
            match result {
                Ok(Ok(()) | Err(XllError::Closing)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(XllError::Panic),
            }
        }
        Ok(())
    }
}

impl Drop for RtdChannelSubscription {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

// SAFETY: only the shared publisher receives a sink. Every shutdown path
// waits on the publication lock and clears the sink before returning. Any
// surviving sender owns only the channel, which is independent of the runtime.
unsafe impl RtdSubscription for RtdChannelSubscription {
    fn request_cancel(&self) {
        self.channel.close();
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.finish()
    }
}

// SAFETY: before spawning user work a local subscription owns the sink
// revocation barrier. Failure/unwind waits for publication and revokes the
// sink. On success the same guard becomes the returned subscription. User
// code receives no sink. The generation separately owns the shared worker.
unsafe impl<T: IntoRtdValue + Send + 'static> RtdSource for RtdChannelSource<T> {
    type Value = T;
    type Subscription = RtdChannelSubscription;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<T>) -> XllResult<Self::Subscription> {
        let permit = ProducerSlots::reserve(&self.producers)?;
        let producer = (self.factory)(topic.clone())?;
        let channel = Arc::new(Channel::new(self.capacity));
        let mut subscription = RtdChannelSubscription::start_publisher(Arc::clone(&channel), sink)?;
        subscription.producer_permit = Some(permit);
        let topic = topic.clone();
        let error_policy = self.error_policy;
        subscription.producer = Some(
            Builder::new()
                .name("xlfn-rtd-producer".into())
                .spawn(move || {
                    let result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                        producer(Channel::sender(&channel))
                    }))
                    .unwrap_or(Err(XllError::Panic));
                    Channel::producer_completed(&channel, &topic, result, error_policy);
                })
                .map_err(spawn_error)?,
        );
        Ok(subscription)
    }
}

fn spawn_error(error: std::io::Error) -> XllError {
    XllError::Native {
        code: error.raw_os_error().unwrap_or(0),
        message: format!("failed to start RTD worker: {error}"),
    }
}

/// Queue-only probe: conversion, enqueue, wakeup, and receive; no RTD sink.
#[cfg(feature = "bench-internals")]
pub fn channel_protocol_probe(
    producers: usize,
    capacity: usize,
    operations: usize,
) -> serde_json::Value {
    use std::time::Instant;
    assert!(producers > 0 && operations > 0);
    let mut rounds = Vec::new();
    let mut overloaded = 0;
    for round in 0..7 {
        let channel = Arc::new(Channel::new(NonZeroUsize::new(capacity).unwrap()));
        let barrier = Arc::new(std::sync::Barrier::new(producers + 2));
        let receiver_channel = Arc::clone(&channel);
        let receiver_barrier = Arc::clone(&barrier);
        let receiver = std::thread::spawn(move || {
            receiver_barrier.wait();
            let mut count = 0;
            let mut batch = smallvec::SmallVec::new();
            while receiver_channel.receive_batch(&mut batch) {
                for value in batch.drain(..) {
                    if receiver_channel.stopping.load(Ordering::Acquire) {
                        break;
                    }
                    std::hint::black_box(value);
                    count += 1;
                }
            }
            count
        });
        let mut workers = Vec::new();
        for _ in 0..producers {
            let sender = Channel::sender::<i32>(&channel);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let mut retries = 0;
                for value in 0..operations {
                    loop {
                        match sender.try_send(value as i32) {
                            Ok(()) => break,
                            Err(RtdSendError::Full(_)) => {
                                retries += 1;
                                std::thread::yield_now();
                            }
                            Err(error) => panic!("unexpected send error: {error}"),
                        }
                    }
                }
                retries
            }));
        }
        let started = Instant::now();
        barrier.wait();
        let retries: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        channel.producer_finished();
        assert_eq!(receiver.join().unwrap(), producers * operations);
        let elapsed = started.elapsed().as_nanos() as u64;
        if round > 0 {
            rounds.push(elapsed);
            overloaded += retries;
        }
    }
    rounds.sort_unstable();
    serde_json::json!({
        "producers": producers, "capacity": capacity, "values_per_round": producers * operations,
        "measured_rounds": rounds.len(), "median_round_ns": (rounds[2] + rounds[3]) / 2,
        "min_round_ns": rounds[0], "max_round_ns": rounds[5], "overloaded_retries": overloaded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::{
        RefreshOutcome, RtdValue, SubscriptionRuntime, SubscriptionServerHandle, TopicId,
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc as StdArc, mpsc};

    const DEADLINE: Duration = Duration::from_secs(5);

    fn capacity() -> NonZeroUsize {
        NonZeroUsize::new(2).unwrap()
    }

    // The runtime stays alive until after the adapter's subscription is joined.
    fn sink() -> (
        Arc<SubscriptionRuntime>,
        SubscriptionServerHandle,
        RtdSink<i32>,
    ) {
        let (arena, source, captured, _) =
            crate::subscription::tests::publishing_source::<i32>(None);
        let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(
                &source,
                RtdTopic::single("channel-test").unwrap().borrowed(),
            )
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, TopicId(1), id)
            .unwrap()
            .commit()
            .unwrap();
        let sink = captured.lock().clone().unwrap();
        (runtime, server, sink)
    }

    fn wait_drained(channel: &Channel) {
        let mut state = channel.state.lock();
        while !state.stopping
            && (state.accepting
                || !state.values.is_empty()
                || state.terminal_error.is_some()
                || state.scheduled)
        {
            #[cfg(miri)]
            channel.changed.wait(&mut state);
            #[cfg(not(miri))]
            assert!(
                !channel.changed.wait_for(&mut state, DEADLINE).timed_out(),
                "publisher stalled"
            );
        }
    }

    fn with_producer_diagnostics(test: impl FnOnce(mpsc::Receiver<XllError>)) {
        struct Capture(mpsc::Sender<XllError>);
        impl crate::diagnostics::DiagnosticSink for Capture {
            fn report(&self, event: &crate::diagnostics::DiagnosticEvent<'_>) {
                // Other tests may fail their own producers while this
                // process-wide test sink is installed.
                if let XllError::RtdProducerFailure { topic, .. } = event.error()
                    && topic.contains("diagnostic-channel-")
                {
                    self.0.send(event.error().clone()).unwrap();
                }
            }
        }

        let _module_lease = crate::ingress::acquire_test_module_lease();
        let _diagnostic_lock = crate::diagnostics::DIAGNOSTIC_TEST_MUTEX.lock();
        let ingress = crate::module_runtime::ingress();
        if ingress.phase() != crate::ingress::PHASE_CLOSED {
            ingress.begin_close_with(|| {});
            let _ = ingress.seal_and_drain();
        }
        let _ = crate::diagnostics::close_diagnostic_router();
        ingress.begin_opening();
        crate::diagnostics::reset_diagnostic_router().unwrap();
        let (events_tx, events_rx) = mpsc::channel();
        crate::diagnostics::set_diagnostic_sink(Capture(events_tx)).unwrap();
        let _cleanup = scopeguard::guard((), |_| {
            let _ = crate::diagnostics::clear_diagnostic_sink();
            ingress.begin_close_with(|| {});
            let _ = ingress.seal_and_drain();
        });
        test(events_rx);
    }

    #[test]
    fn producer_failure_is_diagnosed_and_final_policy_applied_before_disconnect() {
        with_producer_diagnostics(|events| {
            for panic in [false, true] {
                for policy in [
                    RtdProducerErrorPolicy::RetainLastValue,
                    RtdProducerErrorPolicy::PublishError(ExcelError::NotAvailable),
                ] {
                    let (_runtime, server, sink) = sink();
                    let (sender_tx, sender_rx) = mpsc::channel();
                    let source = RtdChannelSource::new(capacity(), move |_| {
                        let sender_tx = sender_tx.clone();
                        Ok(move |sender: RtdSender<i32>| {
                            sender.try_send(42).unwrap();
                            sender_tx.send(sender).unwrap();
                            assert!(!panic, "injected producer failure");
                            Err(XllError::Overloaded)
                        })
                    })
                    .with_error_policy(policy);
                    let subscription = source
                        .subscribe(
                            &RtdTopic::single("diagnostic-channel-policy").unwrap(),
                            sink,
                        )
                        .unwrap();
                    let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
                    assert!(sender.wait_closed(DEADLINE));
                    let event = events.recv_timeout(DEADLINE).unwrap();
                    let XllError::RtdProducerFailure { topic, source } = event else {
                        panic!("missing producer diagnostic context");
                    };
                    assert_eq!(topic, "[\"diagnostic-channel-policy\"]");
                    if panic {
                        assert!(matches!(*source, XllError::Panic));
                    } else {
                        assert!(matches!(*source, XllError::Overloaded));
                    }
                    wait_drained(&subscription.channel);
                    let batch = server.begin_refresh().unwrap();
                    let expected = match policy {
                        RtdProducerErrorPolicy::RetainLastValue => StoredRtdValue::Integer(42),
                        RtdProducerErrorPolicy::PublishError(error) => StoredRtdValue::Error(error),
                    };
                    assert_eq!(batch.updates[0].value, expected);
                    batch.complete(RefreshOutcome::Delivered).unwrap();
                    Box::new(subscription).disconnect_and_wait().unwrap();
                }
            }
            crate::diagnostics::clear_diagnostic_sink().unwrap();
            assert!(
                events.try_recv().is_err(),
                "producer failure was diagnosed twice"
            );
        });
    }

    #[test]
    fn producer_terminal_error_follows_full_queue_across_publisher_turns() {
        let (_runtime, server, sink) = sink();
        let queue = Arc::new(PublisherQueue {
            ready: Mutex::new(Ready {
                channels: VecDeque::new(),
                stopping: false,
            }),
            changed: Condvar::new(),
        });
        let channel = Arc::new(Channel::new(NonZeroUsize::new(64).unwrap()));
        channel.publication.lock().sink = Some(sink.erased());
        *channel.scheduler.lock() = Some(Arc::clone(&queue));
        for value in 0..64 {
            Channel::sender::<i32>(&channel).try_send(value).unwrap();
        }
        Channel::producer_completed(
            &channel,
            &RtdTopic::single("full-queue-failure").unwrap(),
            Err(XllError::Overloaded),
            RtdProducerErrorPolicy::PublishError(ExcelError::NotAvailable),
        );
        for expected in [
            StoredRtdValue::Integer(31),
            StoredRtdValue::Integer(63),
            StoredRtdValue::Error(crate::ExcelError::NotAvailable),
        ] {
            let ready = queue.ready.lock().channels.pop_front().unwrap();
            Channel::publish_batch(&ready);
            let batch = server.begin_refresh().unwrap();
            assert_eq!(batch.updates[0].value, expected);
            batch.complete(RefreshOutcome::Delivered).unwrap();
        }
        assert!(queue.ready.lock().channels.is_empty());
        channel.close();
        channel.publication.lock().sink.take();
        channel.scheduler.lock().take();
    }

    #[test]
    fn cancellation_closing_is_normal_but_unsolicited_closing_is_diagnosed() {
        with_producer_diagnostics(|events| {
            let (_runtime, server, sink) = sink();
            let (sender_tx, sender_rx) = mpsc::channel();
            let source = RtdChannelSource::new(capacity(), move |_| {
                let sender_tx = sender_tx.clone();
                Ok(move |sender: RtdSender<i32>| {
                    sender_tx.send(sender.clone()).unwrap();
                    sender.wait_closed(DEADLINE);
                    Err(XllError::Closing)
                })
            })
            .with_error_policy(RtdProducerErrorPolicy::PublishError(
                ExcelError::NotAvailable,
            ));
            let subscription = source
                .subscribe(
                    &RtdTopic::single("diagnostic-channel-cancel").unwrap(),
                    sink.clone(),
                )
                .unwrap();
            sender_rx.recv_timeout(DEADLINE).unwrap();
            Box::new(subscription).disconnect_and_wait().unwrap();
            let cancelled = server.begin_refresh().unwrap();
            assert!(cancelled.updates.is_empty());
            cancelled.complete(RefreshOutcome::Delivered).unwrap();

            let source = RtdChannelSource::new(capacity(), |_| {
                Ok(|_: RtdSender<i32>| Err(XllError::Closing))
            })
            .with_error_policy(RtdProducerErrorPolicy::PublishError(
                ExcelError::NotAvailable,
            ));
            let subscription = source
                .subscribe(
                    &RtdTopic::single("diagnostic-channel-unexpected-close").unwrap(),
                    sink,
                )
                .unwrap();
            let event = events.recv_timeout(DEADLINE).unwrap();
            assert!(matches!(
                event,
                XllError::RtdProducerFailure { source, .. } if matches!(*source, XllError::Closing)
            ));
            wait_drained(&subscription.channel);
            let failure = server.begin_refresh().unwrap();
            assert_eq!(
                failure.updates[0].value,
                StoredRtdValue::Error(crate::ExcelError::NotAvailable)
            );
            failure.complete(RefreshOutcome::Delivered).unwrap();
            Box::new(subscription).disconnect_and_wait().unwrap();
            crate::diagnostics::clear_diagnostic_sink().unwrap();
            assert!(
                events.try_recv().is_err(),
                "cancellation was diagnosed as failure"
            );
        });
    }

    #[test]
    fn producer_limit_counts_completed_unjoined_workers_and_survives_reconfiguration() {
        let (_runtime, _server, sink) = sink();
        let calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = StdArc::clone(&calls);
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_| {
            called.fetch_add(1, Ordering::Relaxed);
            let sender_tx = sender_tx.clone();
            Ok(move |sender: RtdSender<i32>| {
                sender_tx.send(sender).unwrap();
                Ok(())
            })
        })
        .with_max_producers(NonZeroUsize::new(1).unwrap());
        let topic = RtdTopic::single("bounded-producer").unwrap();
        let first = source.subscribe(&topic, sink.clone()).unwrap();
        assert!(
            sender_rx
                .recv_timeout(DEADLINE)
                .unwrap()
                .wait_closed(DEADLINE)
        );
        let deadline = std::time::Instant::now() + DEADLINE;
        while !first.producer.as_ref().unwrap().is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "producer did not exit"
            );
            std::thread::yield_now();
        }
        // Reconfiguring the source must not reset its retained reservations.
        let source = source.with_max_producers(NonZeroUsize::new(1).unwrap());
        assert!(matches!(
            source.subscribe(&topic, sink.clone()),
            Err(XllError::Overloaded)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        Box::new(first).disconnect_and_wait().unwrap();
        let second = source.subscribe(&topic, sink).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        Box::new(second).disconnect_and_wait().unwrap();
    }

    #[test]
    fn generation_pool_stop_is_terminal_and_joins_its_worker() {
        let pool = PublisherPool::new();
        let queue = pool.queue().unwrap();
        pool.stop();
        assert!(queue.ready.lock().stopping);
        assert!(matches!(pool.queue(), Err(XllError::Closing)));
        pool.stop();
    }

    #[test]
    fn miri_shared_publisher_contains_topic_panic_and_survives_disconnect() {
        let (_runtime, server, sink) = sink();
        let first = Arc::new(Channel::new(capacity()));
        first.panic_publish.store(true, Ordering::Relaxed);
        let first_sub =
            RtdChannelSubscription::start_publisher(Arc::clone(&first), sink.clone()).unwrap();
        let second = Arc::new(Channel::new(capacity()));
        let second_sub =
            RtdChannelSubscription::start_publisher(Arc::clone(&second), sink).unwrap();
        assert!(Arc::ptr_eq(
            first.scheduler.lock().as_ref().unwrap(),
            second.scheduler.lock().as_ref().unwrap()
        ));
        Channel::sender::<i32>(&first).try_send(1).unwrap();
        first.producer_finished();
        wait_drained(&first);
        assert!(matches!(
            Box::new(first_sub).disconnect_and_wait(),
            Err(XllError::Panic)
        ));
        Channel::sender::<i32>(&second).try_send(42).unwrap();
        second.producer_finished();
        wait_drained(&second);
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(42));
        batch.complete(RefreshOutcome::Delivered).unwrap();
        Box::new(second_sub).disconnect_and_wait().unwrap();
    }

    #[test]
    fn shared_publisher_requeues_busy_topic_behind_other_ready_topics() {
        let (_runtime, _server, sink) = sink();
        let queue = Arc::new(PublisherQueue {
            ready: Mutex::new(Ready {
                channels: VecDeque::new(),
                stopping: false,
            }),
            changed: Condvar::new(),
        });
        let channels: Vec<_> = (0..2)
            .map(|_| {
                let channel = Arc::new(Channel::new(NonZeroUsize::new(65).unwrap()));
                channel.publication.lock().sink = Some(sink.erased());
                *channel.scheduler.lock() = Some(Arc::clone(&queue));
                for value in 0..65 {
                    Channel::sender::<i32>(&channel).try_send(value).unwrap();
                }
                channel
            })
            .collect();
        let first = queue.ready.lock().channels.pop_front().unwrap();
        Channel::publish_batch(&first);
        let mut ready = queue.ready.lock();
        assert_eq!(channels[0].state.lock().values.len(), 33);
        assert!(Arc::ptr_eq(
            &ready.channels.pop_front().unwrap(),
            &channels[1]
        ));
        assert!(Arc::ptr_eq(
            &ready.channels.pop_front().unwrap(),
            &channels[0]
        ));
        drop(ready);
        for channel in channels {
            channel.close();
            channel.publication.lock().sink = None;
            channel.scheduler.lock().take();
        }
    }

    #[test]
    fn miri_failed_send_preserves_non_clone_payload_and_converts_once() {
        struct OnceValue {
            conversions: StdArc<std::sync::atomic::AtomicUsize>,
            value: i32,
        }
        impl IntoRtdValue for OnceValue {
            fn into_rtd_value(self) -> XllResult<RtdValue> {
                self.conversions.fetch_add(1, Ordering::Relaxed);
                Ok(RtdValue::Integer(self.value))
            }
        }
        let conversions = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        let channel = Arc::new(Channel::new(NonZeroUsize::new(1).unwrap()));
        let sender = Channel::sender::<OnceValue>(&channel);
        sender
            .try_send(OnceValue {
                conversions: StdArc::clone(&conversions),
                value: 1,
            })
            .unwrap();
        let error = sender
            .try_send(OnceValue {
                conversions: StdArc::clone(&conversions),
                value: 2,
            })
            .unwrap_err();
        assert!(matches!(error, RtdSendError::Full(_)));
        let pending = error.into_pending().unwrap();
        assert_eq!(channel.receive(), Some(StoredRtdValue::Integer(1)));
        sender.try_send_pending(pending).unwrap();
        assert_eq!(channel.receive(), Some(StoredRtdValue::Integer(2)));
        assert_eq!(conversions.load(Ordering::Relaxed), 2);
        channel.close();
        let pending = sender
            .try_send(OnceValue {
                conversions: StdArc::clone(&conversions),
                value: 3,
            })
            .unwrap_err()
            .into_pending()
            .unwrap();
        let other = Arc::new(Channel::new(capacity()));
        Channel::sender::<OnceValue>(&other)
            .try_send_pending(pending)
            .unwrap();
        assert_eq!(other.receive(), Some(StoredRtdValue::Integer(3)));
        assert_eq!(conversions.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn bounded_channel_validates_before_admission() {
        let channel = Arc::new(Channel::new(capacity()));
        let sender = Channel::sender::<f64>(&channel);
        assert!(sender.try_send(f64::NAN).is_err());
        assert!(channel.state.lock().values.is_empty());
        sender.try_send(1.0).unwrap();
        sender.try_send(2.0).unwrap();
        assert!(matches!(sender.try_send(3.0), Err(RtdSendError::Full(_))));
        assert_eq!(channel.receive(), Some(StoredRtdValue::Number(1.0)));
        assert_eq!(channel.receive(), Some(StoredRtdValue::Number(2.0)));
        channel.close();
        assert!(sender.is_closed());
        assert!(matches!(sender.try_send(3.0), Err(RtdSendError::Closed(_))));
        assert_eq!(channel.receive(), None);
    }

    #[derive(Clone)]
    struct UnvalidatedCandidate(RtdValue);

    impl IntoRtdValue for UnvalidatedCandidate {
        fn into_rtd_value(self) -> XllResult<RtdValue> {
            Ok(self.0)
        }
    }

    #[test]
    fn custom_conversion_cannot_bypass_publication_validation() {
        let channel = Arc::new(Channel::new(capacity()));
        let sender = Channel::sender::<UnvalidatedCandidate>(&channel);
        let (_runtime, server, sink) = crate::subscription::tests::connected_sink::<
            UnvalidatedCandidate,
        >(None, "custom-value-validation");
        for invalid in [
            RtdValue::Number(f64::NAN),
            RtdValue::Number(f64::INFINITY),
            RtdValue::Number(f64::NEG_INFINITY),
            RtdValue::String("a".repeat(32_768)),
            RtdValue::String("😀".repeat(16_384)),
        ] {
            assert!(
                sender
                    .try_send(UnvalidatedCandidate(invalid.clone()))
                    .is_err()
            );
            assert!(sink.publish(UnvalidatedCandidate(invalid)).is_err());
        }
        assert!(channel.state.lock().values.is_empty());
        let rejected = server.begin_refresh().unwrap();
        assert!(rejected.updates.is_empty());
        rejected.complete(RefreshOutcome::Delivered).unwrap();

        // The boundary counts UTF-16 units rather than UTF-8 bytes, and a
        // rejected candidate must not prevent the next valid publication.
        let valid = format!("{}a", "😀".repeat(16_383));
        sender
            .try_send(UnvalidatedCandidate(RtdValue::String(valid.clone())))
            .unwrap();
        sink.publish(UnvalidatedCandidate(RtdValue::String(valid.clone())))
            .unwrap();
        assert_eq!(
            channel.receive(),
            Some(StoredRtdValue::String(valid.clone().into_boxed_str())),
        );
        let accepted = server.begin_refresh().unwrap();
        assert_eq!(accepted.updates.len(), 1);
        assert_eq!(
            accepted.updates[0].value,
            StoredRtdValue::String(valid.into_boxed_str()),
        );
        accepted.complete(RefreshOutcome::Delivered).unwrap();
    }

    #[test]
    fn data_wakeup_reaches_publisher_with_cancellation_waiters() {
        let channel = Arc::new(Channel::new(capacity()));
        let mut cancellation = Vec::new();
        for _ in 0..4 {
            let waiting = Arc::clone(&channel);
            let (ready_tx, ready_rx) = mpsc::channel();
            cancellation.push(std::thread::spawn(move || {
                let mut state = waiting.state.lock();
                ready_tx.send(()).unwrap();
                waiting
                    .closed
                    .wait_while_for(&mut state, |state| state.accepting, DEADLINE);
                assert!(!state.accepting);
            }));
            ready_rx.recv_timeout(DEADLINE).unwrap();
            // Acquiring this lock proves the cancellation waiter registered
            // its wait and released the mutex before the publisher starts.
            drop(channel.state.lock());
        }
        let receiving = Arc::clone(&channel);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let publisher = std::thread::spawn(move || {
            let mut state = receiving.state.lock();
            ready_tx.send(()).unwrap();
            receiving
                .changed
                .wait_while_for(&mut state, |state| state.values.is_empty(), DEADLINE);
            drop(state);
            done_tx.send(receiving.receive()).unwrap();
        });
        ready_rx.recv_timeout(DEADLINE).unwrap();
        drop(channel.state.lock());
        Channel::sender::<i32>(&channel).try_send(42).unwrap();
        assert_eq!(
            done_rx.recv_timeout(DEADLINE).unwrap(),
            Some(StoredRtdValue::Integer(42))
        );
        channel.close();
        publisher.join().unwrap();
        for waiter in cancellation {
            waiter.join().unwrap();
        }
    }

    struct CloseDuringConversion(Arc<Channel>);

    impl IntoRtdValue for CloseDuringConversion {
        fn into_rtd_value(self) -> XllResult<RtdValue> {
            // Re-entering the same queue would deadlock if conversion ran
            // under the queue mutex. The later admission check must see close.
            self.0.close();
            Ok(RtdValue::Integer(1))
        }
    }

    #[test]
    fn conversion_can_reenter_and_close_the_channel() {
        let channel = Arc::new(Channel::new(capacity()));
        let sender = Channel::sender::<CloseDuringConversion>(&channel);
        assert!(matches!(
            sender.try_send(CloseDuringConversion(Arc::clone(&channel))),
            Err(RtdSendError::Closed(_))
        ));
        assert!(channel.state.lock().values.is_empty());
    }

    #[test]
    fn miri_channel_batches_preserve_fifo_and_finite_drain() {
        let channel = Arc::new(Channel::new(NonZeroUsize::new(65).unwrap()));
        for value in 0..65 {
            Channel::sender::<i32>(&channel).try_send(value).unwrap();
        }
        channel.producer_finished();
        let mut batch = smallvec::SmallVec::new();
        let mut received = Vec::new();
        let mut sizes = Vec::new();
        while channel.receive_batch(&mut batch) {
            sizes.push(batch.len());
            received.extend(batch.drain(..));
        }
        assert_eq!(sizes, [32, 32, 1]);
        assert_eq!(
            received,
            (0..65).map(StoredRtdValue::Integer).collect::<Vec<_>>()
        );
    }

    #[test]
    fn miri_channel_finite_publisher_drains_all_batches_to_publish_core() {
        let (_runtime, server, sink) = sink();
        let channel = Arc::new(Channel::new(NonZeroUsize::new(65).unwrap()));
        for value in 0..65 {
            Channel::sender::<i32>(&channel).try_send(value).unwrap();
        }
        channel.producer_finished();
        let subscription = RtdChannelSubscription::start_publisher(channel, sink).unwrap();
        {
            let mut state = subscription.channel.state.lock();
            while state.accepting || !state.values.is_empty() || state.scheduled {
                subscription.channel.changed.wait(&mut state);
            }
        }
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(64));
        assert_eq!(batch.updates[0].sequence, 64);
        batch.complete(RefreshOutcome::Delivered).unwrap();
        Box::new(subscription).disconnect_and_wait().unwrap();
    }

    #[test]
    fn miri_channel_cancel_discards_local_batch_after_in_flight_publish() {
        let (_runtime, server, sink) = sink();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let notifier = StdArc::new(crate::rtd::test_support::TestNotifierState::new());
        *notifier.entered.lock() = Some(entered_tx);
        *notifier.release.lock() = Some(release_rx);
        server
            .attach_update_notifier(crate::excel_rtd::RtdNotifier::for_test(notifier))
            .unwrap();
        let channel = Arc::new(Channel::new(NonZeroUsize::new(64).unwrap()));
        for value in 0..64 {
            Channel::sender::<i32>(&channel).try_send(value).unwrap();
        }
        let subscription =
            RtdChannelSubscription::start_publisher(Arc::clone(&channel), sink).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(channel.state.lock().values.len(), 32);
        subscription.request_cancel();
        release_tx.send(()).unwrap();
        drop(release_tx);
        Box::new(subscription).disconnect_and_wait().unwrap();
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(0));
        assert_eq!(batch.updates[0].sequence, 0);
        batch.complete(RefreshOutcome::Delivered).unwrap();
        assert!(channel.state.lock().values.is_empty());
    }

    #[test]
    fn completed_producer_drains_values_then_stops_publisher() {
        let (_runtime, server, sink) = sink();
        let source = RtdChannelSource::new(capacity(), |_| {
            Ok(|sender: RtdSender<i32>| {
                sender.try_send(17).map_err(RtdSendError::into_error)?;
                sender.try_send(42).map_err(RtdSendError::into_error)
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("finite").unwrap(), sink)
            .unwrap();
        {
            let mut state = subscription.channel.state.lock();
            while state.accepting || !state.values.is_empty() || state.scheduled {
                subscription.channel.changed.wait(&mut state);
            }
        }
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(42));
        batch.complete(RefreshOutcome::Delivered).unwrap();
        Box::new(subscription).disconnect_and_wait().unwrap();
    }

    #[test]
    fn miri_factory_and_job_have_independent_unique_lifetimes() {
        struct Owner {
            label: &'static str,
            dropped: mpsc::Sender<&'static str>,
        }

        impl Owner {
            fn new_job(&self) -> Self {
                Self {
                    label: "job",
                    dropped: self.dropped.clone(),
                }
            }
        }

        impl Drop for Owner {
            fn drop(&mut self) {
                let _ = self.dropped.send(self.label);
            }
        }

        let (_runtime, _server, sink) = sink();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let factory_owner = Owner {
            label: "factory",
            dropped: dropped_tx,
        };
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_| {
            // Cell is Send but not Sync. This uncloneable resource belongs
            // only to its FnOnce job, never to a shared producer callback.
            let job_owner = std::cell::Cell::new(Some(factory_owner.new_job()));
            let sender_tx = sender_tx.clone();
            Ok(move |sender: RtdSender<i32>| {
                sender_tx.send(sender.clone()).unwrap();
                // Ownership is independent of the timed-wait adapter, whose
                // platform clock syscall is unavailable in Miri isolation.
                while !sender.is_closed() {
                    std::thread::yield_now();
                }
                drop(job_owner.take());
                Ok(())
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("owned-job").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();

        drop(source);

        assert_eq!(dropped_rx.recv_timeout(DEADLINE).unwrap(), "factory");
        assert!(dropped_rx.try_recv().is_err());
        assert!(!sender.is_closed());
        sender.try_send(42).unwrap();

        Box::new(subscription).disconnect_and_wait().unwrap();

        assert_eq!(dropped_rx.try_recv().unwrap(), "job");
        assert!(sender.is_closed());
        assert!(sender.channel.state.lock().values.is_empty());
    }

    #[test]
    fn factory_error_or_panic_fails_subscription_setup_synchronously() {
        for panic in [false, true] {
            let (_runtime, _server, sink) = sink();
            let factory_calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
            let source = RtdChannelSource::new(
                capacity(),
                move |topic| -> XllResult<fn(RtdSender<i32>) -> XllResult<()>> {
                    assert!(topic.parts().eq(["invalid"]));
                    if factory_calls.fetch_add(1, Ordering::Relaxed) > 0 {
                        return Ok(|_| Ok(()));
                    }
                    if panic {
                        panic!("injected factory panic");
                    }
                    Err(XllError::Overloaded)
                },
            )
            .with_max_producers(NonZeroUsize::new(1).unwrap());
            let result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                source.subscribe(&RtdTopic::single("invalid").unwrap(), sink.clone())
            }));
            if panic {
                assert!(result.is_err());
            } else {
                assert!(matches!(result, Ok(Err(XllError::Overloaded))));
            }
            // Error and unwind must release their pre-factory reservation.
            let retry = source
                .subscribe(&RtdTopic::single("invalid").unwrap(), sink)
                .unwrap();
            Box::new(retry).disconnect_and_wait().unwrap();
        }
    }

    #[test]
    fn disconnect_joins_producer_and_closes_escaped_senders() {
        let (sender_tx, sender_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let finished = Arc::new(AtomicBool::new(false));
        let producer_finished = Arc::clone(&finished);
        let source = RtdChannelSource::new(capacity(), move |_| {
            let sender_tx = sender_tx.clone();
            let closed_tx = closed_tx.clone();
            let release_rx = release_rx.lock().take().unwrap();
            let producer_finished = Arc::clone(&producer_finished);
            Ok(move |sender: RtdSender<i32>| {
                sender_tx.send(sender.clone()).unwrap();
                assert!(sender.wait_closed(DEADLINE));
                closed_tx.send(()).unwrap();
                release_rx.recv_timeout(DEADLINE).unwrap();
                producer_finished.store(true, Ordering::Release);
                Ok(())
            })
        });
        let (arena, source) = crate::subscription::SourceArena::with_source(
            crate::generation::RuntimeGeneration::new(1).unwrap(),
            source,
        )
        .unwrap();
        let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(&source, RtdTopic::single("disconnect").unwrap().borrowed())
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, TopicId(1), id)
            .unwrap()
            .commit()
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        let sender_clone = sender.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let disconnect_runtime = Arc::clone(&runtime);
        let disconnect = std::thread::spawn(move || {
            let result = disconnect_runtime.disconnect(&server, TopicId(1));
            done_tx.send(result).unwrap();
        });
        closed_rx.recv_timeout(DEADLINE).unwrap();
        assert!(done_rx.try_recv().is_err());
        assert!(matches!(sender.try_send(1), Err(RtdSendError::Closed(_))));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(DEADLINE).unwrap().unwrap();
        disconnect.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        drop(runtime);
        assert!(sender_clone.is_closed());
        assert!(matches!(
            sender_clone.try_send(2),
            Err(RtdSendError::Closed(_))
        ));
    }

    #[test]
    fn disconnect_waits_for_in_flight_publisher() {
        let (_runtime, server, sink) = sink();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let notifier = StdArc::new(crate::rtd::test_support::TestNotifierState::new());
        *notifier.entered.lock() = Some(entered_tx);
        *notifier.release.lock() = Some(release_rx);
        server
            .attach_update_notifier(crate::excel_rtd::RtdNotifier::for_test(notifier))
            .unwrap();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_| {
            let sender_tx = sender_tx.clone();
            Ok(move |sender: RtdSender<i32>| {
                sender_tx.send(sender.clone()).unwrap();
                sender.try_send(1).map_err(RtdSendError::into_error)?;
                assert!(sender.wait_closed(DEADLINE));
                Ok(())
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("blocked-publisher").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let disconnect = std::thread::spawn(move || {
            done_tx
                .send(Box::new(subscription).disconnect_and_wait())
                .unwrap();
        });
        assert!(sender.wait_closed(DEADLINE));
        assert!(done_rx.try_recv().is_err());
        assert!(matches!(sender.try_send(2), Err(RtdSendError::Closed(_))));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(DEADLINE).unwrap().unwrap();
        disconnect.join().unwrap();
    }

    #[test]
    fn panic_in_producer_closes_sender_without_repeating_failure_on_disconnect() {
        let (_runtime, _server, sink) = sink();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_| {
            let sender_tx = sender_tx.clone();
            Ok(move |sender| {
                sender_tx.send(sender).unwrap();
                panic!("injected producer panic");
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("panic").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert!(sender.wait_closed(DEADLINE));
        Box::new(subscription).disconnect_and_wait().unwrap();
        assert!(matches!(sender.try_send(1), Err(RtdSendError::Closed(_))));
    }

    #[test]
    fn producer_error_closes_sender_without_repeating_failure_on_disconnect() {
        let (_runtime, _server, sink) = sink();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_| {
            let sender_tx = sender_tx.clone();
            Ok(move |sender| {
                sender_tx.send(sender).unwrap();
                Err(XllError::Overloaded)
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("error").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert!(sender.wait_closed(DEADLINE));
        Box::new(subscription).disconnect_and_wait().unwrap();
    }

    #[test]
    fn custom_producer_panic_payload_cannot_unwind_disconnect_or_drop() {
        for explicit_disconnect in [false, true] {
            let (_runtime, _server, sink) = sink();
            let payload_drops = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
            let producer_drops = StdArc::clone(&payload_drops);
            let source = RtdChannelSource::new(capacity(), move |_| {
                let producer_drops = StdArc::clone(&producer_drops);
                Ok(move |_| {
                    std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(
                        producer_drops,
                    ));
                })
            });
            let subscription = source
                .subscribe(&RtdTopic::single("custom-payload").unwrap(), sink)
                .unwrap();
            if explicit_disconnect {
                Box::new(subscription).disconnect_and_wait().unwrap();
            } else {
                drop(subscription);
            }
            assert_eq!(payload_drops.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn unwinding_owner_joins_workers_before_losing_subscription() {
        let (_runtime, _server, sink) = sink();
        let (sender_tx, sender_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let producer_finished = Arc::clone(&finished);
        let source = RtdChannelSource::new(capacity(), move |_| {
            let sender_tx = sender_tx.clone();
            let producer_finished = Arc::clone(&producer_finished);
            Ok(move |sender: RtdSender<i32>| {
                sender_tx.send(sender.clone()).unwrap();
                assert!(sender.wait_closed(DEADLINE));
                producer_finished.store(true, Ordering::Release);
                Ok(())
            })
        });
        let subscription = source
            .subscribe(&RtdTopic::single("unwind").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(move || {
                let _subscription = subscription;
                panic!("unwind after starting publisher");
            }))
            .is_err()
        );
        assert!(finished.load(Ordering::Acquire));
        assert!(matches!(sender.try_send(1), Err(RtdSendError::Closed(_))));
    }

    #[test]
    fn publisher_only_setup_error_and_unwind_join_before_return() {
        for unwind in [false, true] {
            let (runtime, _server, sink) = sink();
            let channel = Arc::new(Channel::new(capacity()));
            let sender = Channel::sender::<i32>(&channel);
            let result = catch_unwind(AssertUnwindSafe(move || {
                let _subscription = RtdChannelSubscription::start_publisher(channel, sink)?;
                // This is the intermediate setup state when spawning the
                // producer fails, or preparation of that spawn unwinds.
                if unwind {
                    panic!("injected failure before producer starts");
                }
                Err::<(), _>(XllError::Overloaded)
            }));
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(matches!(result, Ok(Err(XllError::Overloaded))));
            }
            assert!(sender.is_closed());
            // The publisher's owning channel reference has been released
            // before setup returns, leaving only the retained sender.
            assert_eq!(Arc::count(&sender.channel), 1);
            drop(runtime);
            assert!(matches!(sender.try_send(1), Err(RtdSendError::Closed(_))));
        }
    }
}
