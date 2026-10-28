#![allow(
    unsafe_code,
    reason = "The adapter keeps all sink capabilities inside framework-owned revocable channels"
)]

use super::budget::{
    ChannelBudget, RtdDelivery, RtdDeliveryPolicy, RtdFifo, RtdSourceLimits, SubscriptionPermit,
    WorkerPermit,
};
use super::channel::{RtdChannelSubscription, RtdProducerErrorPolicy, RtdSender};
use super::source::{RtdSink, RtdSource, RtdSubscription};
use super::topic::RtdTopic;
use super::value::IntoRtdValue;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{Builder, JoinHandle};
use std::time::Duration;
use triomphe::Arc;

/// Identifies one connection to a multiplexed source.
///
/// IDs are unique within that source's lifetime. Reconnecting the same topic
/// gets a new ID, so an old removal cannot remove a newer registration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RtdSubscriptionId(u64);

/// A subscription change delivered to a shared producer.
///
/// Events are ordered by registration/removal, including when a subscription
/// disconnects before the producer reads its `Subscribe` event. In that case
/// the event's sender is already closed. Always key removals by ID.
#[non_exhaustive]
pub enum RtdSourceEvent<T, D: RtdDeliveryPolicy = RtdFifo> {
    /// Register a topic on the shared upstream connection.
    Subscribe {
        /// Source-local identity of this connection.
        id: RtdSubscriptionId,
        /// Canonical topic parts.
        topic: RtdTopic,
        /// Revocable, bounded value sender for this connection.
        sender: RtdSender<T, D>,
    },
    /// Remove this connection from the upstream subscription set.
    Unsubscribe {
        /// Identity from the corresponding `Subscribe` event.
        id: RtdSubscriptionId,
    },
}

/// The subscription event stream owned by one shared producer job.
///
/// Use bounded, cancellation-aware I/O and check this context between I/O
/// turns. The producer must return when the context closes and must join any
/// additional threads/callbacks before returning. Neither a context nor a
/// sender can extend the lifetime of the RTD runtime.
pub struct RtdSourceContext<T, D: RtdDeliveryPolicy = RtdFifo> {
    shared: Arc<Shared<T, D>>,
}

impl<T, D: RtdDeliveryPolicy> RtdSourceContext<T, D> {
    /// Returns whether source shutdown or producer completion has closed it.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// Waits for a subscription change, returning `None` on timeout.
    ///
    /// A zero timeout polls without waiting. Source cancellation wakes the
    /// waiter and returns [`XllError::Closing`]. Reading an unsubscribe returns
    /// its reserved control capacity before handing the event to the caller.
    pub fn recv_timeout(&self, timeout: Duration) -> XllResult<Option<RtdSourceEvent<T, D>>> {
        let mut state = self.shared.state.lock();
        self.shared.changed.wait_while_for(
            &mut state,
            |state| !state.closed && state.events.is_empty(),
            timeout,
        );
        if state.closed {
            return Err(XllError::Closing);
        }
        let event = state.events.pop_front();
        if matches!(event, Some(ControlEvent::Unsubscribe { .. })) {
            state.removals -= 1;
        }
        drop(state);
        Ok(event.map(|event| match event {
            ControlEvent::Subscribe { id, topic, sender } => {
                RtdSourceEvent::Subscribe { id, topic, sender }
            }
            ControlEvent::Unsubscribe { id, permit } => {
                drop(permit);
                RtdSourceEvent::Unsubscribe { id }
            }
        }))
    }
}

/// Validates a topic before registration in a multiplexed source.
///
/// Closures implement this trait directly. Sources without a validator retain
/// a zero-sized [`NoTopicValidator`] instead of an optional function pointer.
pub trait RtdTopicValidator: Send + Sync + 'static {
    /// Rejects an invalid topic before its registration starts.
    fn validate(&self, topic: &RtdTopic) -> XllResult<()>;
}

/// A zero-sized policy that accepts every already-parsed topic.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTopicValidator;

impl RtdTopicValidator for NoTopicValidator {
    fn validate(&self, _topic: &RtdTopic) -> XllResult<()> {
        Ok(())
    }
}

impl<F> RtdTopicValidator for F
where
    F: Fn(&RtdTopic) -> XllResult<()> + Send + Sync + 'static,
{
    fn validate(&self, topic: &RtdTopic) -> XllResult<()> {
        self(topic)
    }
}

/// A safe source that multiplexes many topics on one framework-owned worker.
///
/// The job starts lazily on the first admitted subscription and runs once for
/// this source. It owns the shared connection and receives registration and
/// removal events. Subscriptions do not create threads. Their cancellation
/// closes and revokes only their channel; it never joins the shared worker.
/// Runtime shutdown cancels every source before joining any source worker.
///
/// Successful job completion closes source admission and drains accepted
/// values. Errors and panics also apply the configured producer error policy
/// to connected topics. A completed source is not restarted. Exited workers
/// retain their runtime worker reservation until joined at shutdown.
///
/// Source subscription capacity includes removals not yet consumed by the
/// producer, bounding the control backlog even under rapid reconnects. Data
/// buffers are charged to both source and runtime budgets. Upstream buffers,
/// connection resources and application-owned event copies are not included.
/// The producer and validator keep their concrete types until registration in
/// the runtime's heterogeneous source arena.
pub struct RtdMultiplexedSource<T, P, V = NoTopicValidator, D: RtdDeliveryPolicy = RtdFifo> {
    // Join source work before releasing other source fields or user captures.
    worker: WorkerOwner<T, P, D>,
    delivery: D,
    limits: RtdSourceLimits,
    budget: Arc<ChannelBudget>,
    validator: V,
    error_policy: RtdProducerErrorPolicy,
    #[cfg(test)]
    fail_spawn: AtomicBool,
}

enum Worker<P> {
    Unstarted(P),
    // Visible only while the start guard retains the worker mutex. Its Drop
    // restores the producer if thread creation fails or setup unwinds.
    Starting,
    Running {
        handle: JoinHandle<()>,
        permit: WorkerPermit,
    },
    Joining,
    Joined(XllResult<()>),
}

struct WorkerStart<'a, P> {
    worker: Option<&'a mut Worker<P>>,
    producer: Arc<Mutex<Option<P>>>,
}

impl<'a, P> WorkerStart<'a, P> {
    fn reserve(worker: &'a mut Worker<P>) -> Self {
        let Worker::Unstarted(producer) = std::mem::replace(worker, Worker::Starting) else {
            xlfn_kernel::invariant::fail_stop();
        };
        Self {
            worker: Some(worker),
            producer: Arc::new(Mutex::new(Some(producer))),
        }
    }

    fn commit(mut self, handle: JoinHandle<()>, permit: WorkerPermit) {
        *self.worker.take().expect("start guard owns worker state") =
            Worker::Running { handle, permit };
    }
}

impl<P> Drop for WorkerStart<'_, P> {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            // OS spawn failure drops only the closure's Arc. The producer is
            // restored without running user destructors under the worker lock.
            let Some(producer) = self.producer.lock().take() else {
                // A started thread must transfer its join handle immediately;
                // otherwise there is no owner that could establish its barrier.
                xlfn_kernel::invariant::fail_stop();
            };
            *worker = Worker::Unstarted(producer);
        }
    }
}

// Owns the shutdown barrier independently of the validator's concrete type.
// Rebuilding a source in with_topic_validator moves this guard without running
// shutdown or requiring unsafe moves out of a type that implements Drop.
struct WorkerOwner<T, P, D: RtdDeliveryPolicy> {
    shared: Arc<Shared<T, D>>,
    state: Mutex<Worker<P>>,
    started: AtomicBool,
    joined: Condvar,
}

struct Active<T, D: RtdDeliveryPolicy> {
    topic: RtdTopic,
    sender: RtdSender<T, D>,
}

enum ControlEvent<T, D: RtdDeliveryPolicy> {
    Subscribe {
        id: RtdSubscriptionId,
        topic: RtdTopic,
        sender: RtdSender<T, D>,
    },
    Unsubscribe {
        id: RtdSubscriptionId,
        permit: SubscriptionPermit,
    },
}

struct State<T, D: RtdDeliveryPolicy> {
    events: VecDeque<ControlEvent<T, D>>,
    active: FxHashMap<RtdSubscriptionId, Active<T, D>>,
    removals: usize,
    next_id: u64,
    closed: bool,
    stopping: bool,
}

struct Shared<T, D: RtdDeliveryPolicy> {
    state: Mutex<State<T, D>>,
    closed: AtomicBool,
    changed: Condvar,
}

impl<T, D: RtdDeliveryPolicy> Shared<T, D> {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                events: VecDeque::new(),
                active: FxHashMap::default(),
                removals: 0,
                next_id: 0,
                closed: false,
                stopping: false,
            }),
            closed: AtomicBool::new(false),
            changed: Condvar::new(),
        }
    }

    fn register(&self, topic: &RtdTopic, sender: RtdSender<T, D>) -> XllResult<RtdSubscriptionId> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(XllError::Closing);
        }
        let next_id = state.next_id.checked_add(1).ok_or(XllError::Overloaded)?;
        // Each retained subscription needs at most its subscribe event and
        // one future removal. Reserve both before making registration visible;
        // disconnect cannot fail or allocate when transferring its permit.
        let controls = state
            .active
            .len()
            .checked_add(state.removals)
            .and_then(|used| used.checked_add(1))
            .and_then(|used| used.checked_mul(2))
            .ok_or(XllError::Overloaded)?;
        let additional = controls.saturating_sub(state.events.len());
        state
            .events
            .try_reserve(additional)
            .map_err(|_| XllError::Overloaded)?;
        state
            .active
            .try_reserve(1)
            .map_err(|_| XllError::Overloaded)?;
        let id = RtdSubscriptionId(next_id);
        state.next_id = next_id;
        state.active.insert(
            id,
            Active {
                topic: topic.clone(),
                sender: sender.clone(),
            },
        );
        state.events.push_back(ControlEvent::Subscribe {
            id,
            topic: topic.clone(),
            sender,
        });
        drop(state);
        self.changed.notify_one();
        Ok(id)
    }

    fn unregister(&self, id: RtdSubscriptionId, permit: SubscriptionPermit) {
        let mut state = self.state.lock();
        state.active.remove(&id);
        if !state.closed {
            debug_assert!(state.events.len() < state.events.capacity());
            state.removals += 1;
            state
                .events
                .push_back(ControlEvent::Unsubscribe { id, permit });
        }
        drop(state);
        self.changed.notify_one();
    }

    fn stop(&self) {
        let (active, events) = {
            let mut state = self.state.lock();
            state.closed = true;
            state.stopping = true;
            self.closed.store(true, Ordering::Release);
            state.removals = 0;
            (
                std::mem::take(&mut state.active),
                std::mem::take(&mut state.events),
            )
        };
        self.changed.notify_all();
        for registration in active.into_values() {
            registration.sender.cancel();
        }
        drop(events);
    }

    fn complete(&self, result: XllResult<()>, policy: RtdProducerErrorPolicy) {
        let (active, events, stopping) = {
            let mut state = self.state.lock();
            state.closed = true;
            self.closed.store(true, Ordering::Release);
            state.removals = 0;
            (
                std::mem::take(&mut state.active),
                std::mem::take(&mut state.events),
                state.stopping,
            )
        };
        self.changed.notify_all();
        // A producer error is still diagnosed if all topics disconnected
        // before it failed. Cancellation's Closing is a normal completion.
        if active.is_empty()
            && let Err(error) = &result
            && !(stopping && matches!(error, XllError::Closing))
        {
            crate::diagnostics::report_no_unwind("RTD shared producer", error);
        }
        for registration in active.into_values() {
            registration
                .sender
                .complete(&registration.topic, result.clone(), policy);
        }
        drop(events);
    }
}

impl<T: IntoRtdValue + Send + 'static, P, D: RtdDeliveryPolicy>
    RtdMultiplexedSource<T, P, NoTopicValidator, D>
where
    P: FnOnce(RtdSourceContext<T, D>) -> XllResult<()> + Send + 'static,
{
    /// Creates a shared producer without starting it or allocating a thread.
    ///
    /// The job may own resources that are neither `Clone` nor `Sync`.
    pub fn new(delivery: D, producer: P) -> Self {
        let shared = Arc::new(Shared::new());
        Self {
            delivery,
            limits: RtdSourceLimits::standard(),
            budget: Arc::new(ChannelBudget::source(RtdSourceLimits::standard())),
            validator: NoTopicValidator,
            worker: WorkerOwner {
                shared,
                state: Mutex::new(Worker::Unstarted(producer)),
                started: AtomicBool::new(false),
                joined: Condvar::new(),
            },
            error_policy: RtdProducerErrorPolicy::default(),
            #[cfg(test)]
            fail_spawn: AtomicBool::new(false),
        }
    }
}

impl<T, P, V, D> RtdMultiplexedSource<T, P, V, D>
where
    T: IntoRtdValue + Send + 'static,
    P: FnOnce(RtdSourceContext<T, D>) -> XllResult<()> + Send + 'static,
    V: RtdTopicValidator,
    D: RtdDeliveryPolicy,
{
    /// Sets aggregate source bounds, preserving existing reservations.
    ///
    /// Subscription slots include unconsumed removals. Lowering a bound below
    /// retained usage prevents new admission until enough capacity returns.
    #[must_use]
    pub fn with_limits(mut self, limits: RtdSourceLimits) -> Self {
        self.budget.configure_source(limits);
        self.limits = limits;
        self
    }

    /// Validates each topic synchronously before starting its registration.
    ///
    /// Perform only bounded validation here; initialize shared I/O in the job.
    /// Failure rolls back the channel and all subscription reservations. This
    /// builder changes the source's validator type without starting its job.
    #[must_use]
    pub fn with_topic_validator<W>(self, validator: W) -> RtdMultiplexedSource<T, P, W, D>
    where
        W: Fn(&RtdTopic) -> XllResult<()> + Send + Sync + 'static,
    {
        self.with_validator_policy(validator)
    }

    /// Removes validation while preserving the owned job and shutdown barrier.
    #[must_use]
    pub fn without_topic_validator(self) -> RtdMultiplexedSource<T, P, NoTopicValidator, D> {
        self.with_validator_policy(NoTopicValidator)
    }

    fn with_validator_policy<W: RtdTopicValidator>(
        self,
        validator: W,
    ) -> RtdMultiplexedSource<T, P, W, D> {
        RtdMultiplexedSource {
            delivery: self.delivery,
            limits: self.limits,
            budget: self.budget,
            validator,
            worker: self.worker,
            error_policy: self.error_policy,
            #[cfg(test)]
            fail_spawn: self.fail_spawn,
        }
    }

    /// Selects the final cell behavior when the shared producer fails.
    #[must_use]
    pub fn with_error_policy(mut self, policy: RtdProducerErrorPolicy) -> Self {
        self.error_policy = policy;
        self
    }

    /// Returns the data buffering policy.
    #[must_use]
    pub fn delivery(&self) -> RtdDelivery {
        self.delivery.delivery()
    }

    /// Returns the aggregate source limits.
    #[must_use]
    pub const fn limits(&self) -> RtdSourceLimits {
        self.limits
    }

    /// Returns the configured producer failure policy.
    #[must_use]
    pub const fn error_policy(&self) -> RtdProducerErrorPolicy {
        self.error_policy
    }

    fn register_channel(
        &self,
        topic: &RtdTopic,
        channel: super::channel::AttachedChannel<D>,
    ) -> XllResult<RtdMultiplexedSubscription<T, D>> {
        let id = self
            .worker
            .shared
            .register(topic, channel.sender(self.delivery))?;
        let (channel, subscription_permit) = channel.into_multiplexed_parts();
        Ok(RtdMultiplexedSubscription {
            channel,
            shared: Arc::clone(&self.worker.shared),
            id,
            permit: Some(subscription_permit),
        })
    }

    fn start(
        &self,
        topic: &RtdTopic,
        channel: super::channel::AttachedChannel<D>,
        budget: &Arc<ChannelBudget>,
    ) -> XllResult<RtdMultiplexedSubscription<T, D>> {
        if self.worker.started.load(Ordering::Acquire) {
            // stop/complete close shared registration before joining. Its
            // mutex remains the admission authority for every warm subscriber.
            return self.register_channel(topic, channel);
        }
        let mut worker = self.worker.state.lock();
        if self.worker.shared.closed.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        match &*worker {
            Worker::Running { .. } => return self.register_channel(topic, channel),
            Worker::Unstarted(_) => {}
            Worker::Joining | Worker::Joined(_) => return Err(XllError::Closing),
            Worker::Starting => xlfn_kernel::invariant::fail_stop(),
        }
        // Reserve before registering control events: a rejected first worker
        // must return the source slot even when no consumer exists yet.
        let permit = ChannelBudget::try_worker(budget)?;
        let subscription = self.register_channel(topic, channel)?;
        // Retain ownership outside the spawn closure. Failed OS thread creation
        // drops that closure synchronously; it must not destroy user captures
        // while this source's worker mutex is held.
        let starting = WorkerStart::reserve(&mut worker);
        let thread_pending = Arc::clone(&starting.producer);
        let shared = Arc::clone(&self.worker.shared);
        let policy = self.error_policy;
        let run = move || {
            let producer = thread_pending
                .lock()
                .take()
                .expect("one source worker owns the job");
            let context = RtdSourceContext {
                shared: Arc::clone(&shared),
            };
            let result =
                crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| producer(context)))
                    .unwrap_or(Err(XllError::Panic));
            shared.complete(result, policy);
        };
        #[cfg(test)]
        let fail_spawn = self.fail_spawn.swap(false, Ordering::Relaxed);
        #[cfg(not(test))]
        let fail_spawn = false;
        let handle = if fail_spawn {
            drop(run);
            Err(std::io::Error::other("injected RTD source spawn failure"))
        } else {
            Builder::new().name("xlfn-rtd-source".into()).spawn(run)
        };
        match handle {
            Ok(handle) => {
                starting.commit(handle, permit);
                self.worker.started.store(true, Ordering::Release);
                Ok(subscription)
            }
            Err(error) => {
                drop(starting);
                self.worker.shared.stop();
                Err(XllError::Native {
                    code: error.raw_os_error().unwrap_or(0),
                    message: format!("failed to start RTD source worker: {error}"),
                })
            }
        }
    }
}

impl<T, P, D: RtdDeliveryPolicy> WorkerOwner<T, P, D> {
    fn finish(&self) -> XllResult<()> {
        self.shared.stop();
        let retiring = {
            let mut worker = self.state.lock();
            loop {
                match &*worker {
                    Worker::Joining => self.joined.wait(&mut worker),
                    Worker::Joined(result) => return result.clone(),
                    Worker::Unstarted(_) | Worker::Running { .. } => break,
                    Worker::Starting => xlfn_kernel::invariant::fail_stop(),
                }
            }
            std::mem::replace(&mut *worker, Worker::Joining)
        };
        // No registry or worker lock is held while user captures are destroyed
        // or the shared job is joined. Keep the worker reservation through both.
        let result = match retiring {
            Worker::Unstarted(producer) => {
                crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| drop(producer)))
                    .map_err(|_| XllError::Panic)
            }
            Worker::Running { handle, permit } => {
                let result = crate::panic_boundary::contain_panic(handle.join())
                    .map_err(|_| XllError::Panic);
                drop(permit);
                result
            }
            Worker::Starting | Worker::Joining | Worker::Joined(_) => {
                xlfn_kernel::invariant::fail_stop()
            }
        };
        let mut worker = self.state.lock();
        *worker = Worker::Joined(result.clone());
        self.joined.notify_all();
        result
    }
}

impl<T, P, D: RtdDeliveryPolicy> Drop for WorkerOwner<T, P, D> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Framework-owned per-topic revocation barrier for a multiplexed source.
///
/// Disconnect never waits for upstream I/O or joins the source worker. It
/// closes the data queue, waits for in-flight publication, revokes the sink,
/// and transfers a pre-reserved removal event to the shared producer.
pub struct RtdMultiplexedSubscription<T, D: RtdDeliveryPolicy = RtdFifo> {
    channel: RtdChannelSubscription,
    shared: Arc<Shared<T, D>>,
    id: RtdSubscriptionId,
    permit: Option<SubscriptionPermit>,
}

impl<T, D: RtdDeliveryPolicy> RtdMultiplexedSubscription<T, D> {
    fn finish(&mut self) -> XllResult<()> {
        let result = self.channel.finish();
        if let Some(permit) = self.permit.take() {
            self.shared.unregister(self.id, permit);
        }
        result
    }
}

impl<T, D: RtdDeliveryPolicy> Drop for RtdMultiplexedSubscription<T, D> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

// SAFETY: only the channel's shared publisher can access a sink. finish and
// Drop revoke that channel and wait for publication before any exit. The
// producer and any escaped sender clones retain no usable runtime capability.
unsafe impl<T: Send + 'static, D: RtdDeliveryPolicy> RtdSubscription
    for RtdMultiplexedSubscription<T, D>
{
    fn request_cancel(&self) {
        self.channel.request_cancel();
    }
    fn disconnect_and_wait(mut self) -> XllResult<()> {
        self.finish()
    }
}

// SAFETY: an attached channel is locally owned until registration succeeds;
// rollback and unwinding revoke its sink through Drop. The shared producer
// receives only safe senders. Every successful subscription owns its channel
// revocation barrier; source shutdown additionally joins the shared job.
unsafe impl<T, P, V, D> RtdSource for RtdMultiplexedSource<T, P, V, D>
where
    T: IntoRtdValue + Send + 'static,
    P: FnOnce(RtdSourceContext<T, D>) -> XllResult<()> + Send + 'static,
    V: RtdTopicValidator,
    D: RtdDeliveryPolicy,
{
    type Value = T;
    type Subscription = RtdMultiplexedSubscription<T, D>;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<T>) -> XllResult<Self::Subscription> {
        let runtime_budget = sink.channel_budget();
        let channel = super::channel::AttachedChannel::attach(
            self.delivery,
            Arc::clone(&self.budget),
            Arc::clone(&runtime_budget),
            sink,
        )?;
        self.validator.validate(topic)?;
        self.start(topic, channel, &runtime_budget)
    }

    fn request_stop(&self) {
        self.worker.shared.stop();
    }
    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.worker.finish()
    }
}

#[cfg(test)]
mod tests;
