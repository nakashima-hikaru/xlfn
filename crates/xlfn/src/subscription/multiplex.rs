#![allow(
    unsafe_code,
    reason = "The adapter keeps all sink capabilities inside framework-owned revocable channels"
)]

use super::budget::{
    ChannelBudget, RtdDelivery, RtdSourceLimits, SubscriptionPermit, WorkerPermit,
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
pub enum RtdSourceEvent<T> {
    /// Register a topic on the shared upstream connection.
    Subscribe {
        /// Source-local identity of this connection.
        id: RtdSubscriptionId,
        /// Canonical topic parts.
        topic: RtdTopic,
        /// Revocable, bounded value sender for this connection.
        sender: RtdSender<T>,
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
pub struct RtdSourceContext<T> {
    shared: Arc<Shared<T>>,
}

impl<T> RtdSourceContext<T> {
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
    pub fn recv_timeout(&self, timeout: Duration) -> XllResult<Option<RtdSourceEvent<T>>> {
        let mut state = self.shared.state.lock();
        self.shared.changed.wait_while_for(
            &mut state,
            |state| !state.closed && state.events.is_empty(),
            timeout,
        );
        if state.closed {
            return Err(XllError::Closing);
        }
        Ok(state.events.pop_front().map(|event| match event {
            ControlEvent::Subscribe { id, topic, sender } => {
                RtdSourceEvent::Subscribe { id, topic, sender }
            }
            ControlEvent::Unsubscribe { id, permit } => {
                state.removals -= 1;
                drop(permit);
                RtdSourceEvent::Unsubscribe { id }
            }
        }))
    }
}

type Producer<T> = dyn FnOnce(RtdSourceContext<T>) -> XllResult<()> + Send;
type TopicValidator = dyn Fn(&RtdTopic) -> XllResult<()> + Send + Sync;

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
pub struct RtdMultiplexedSource<T> {
    delivery: RtdDelivery,
    limits: RtdSourceLimits,
    budget: Arc<ChannelBudget>,
    validator: Box<TopicValidator>,
    shared: Arc<Shared<T>>,
    worker: Mutex<Worker<T>>,
    joined: Condvar,
    error_policy: RtdProducerErrorPolicy,
    #[cfg(test)]
    fail_spawn: AtomicBool,
}

struct Worker<T> {
    producer: Option<Box<Producer<T>>>,
    handle: Option<JoinHandle<()>>,
    permit: Option<WorkerPermit>,
    joining: bool,
    result: Option<XllResult<()>>,
}

struct Active<T> {
    topic: RtdTopic,
    sender: RtdSender<T>,
}

enum ControlEvent<T> {
    Subscribe {
        id: RtdSubscriptionId,
        topic: RtdTopic,
        sender: RtdSender<T>,
    },
    Unsubscribe {
        id: RtdSubscriptionId,
        permit: SubscriptionPermit,
    },
}

struct State<T> {
    events: VecDeque<ControlEvent<T>>,
    active: FxHashMap<RtdSubscriptionId, Active<T>>,
    removals: usize,
    next_id: u64,
    closed: bool,
    stopping: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    closed: AtomicBool,
    changed: Condvar,
}

impl<T> Shared<T> {
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

    fn register(&self, topic: &RtdTopic, sender: RtdSender<T>) -> XllResult<RtdSubscriptionId> {
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

impl<T: IntoRtdValue + Send + 'static> RtdMultiplexedSource<T> {
    /// Creates a shared producer without starting it or allocating a thread.
    ///
    /// The job may own resources that are neither `Clone` nor `Sync`.
    pub fn new(
        delivery: RtdDelivery,
        producer: impl FnOnce(RtdSourceContext<T>) -> XllResult<()> + Send + 'static,
    ) -> Self {
        Self {
            delivery,
            limits: RtdSourceLimits::standard(),
            budget: Arc::new(ChannelBudget::source(RtdSourceLimits::standard())),
            validator: Box::new(|_| Ok(())),
            shared: Arc::new(Shared::new()),
            worker: Mutex::new(Worker {
                producer: Some(Box::new(producer)),
                handle: None,
                permit: None,
                joining: false,
                result: None,
            }),
            joined: Condvar::new(),
            error_policy: RtdProducerErrorPolicy::default(),
            #[cfg(test)]
            fail_spawn: AtomicBool::new(false),
        }
    }

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
    /// Failure rolls back the channel and all subscription reservations.
    #[must_use]
    pub fn with_topic_validator(
        mut self,
        validator: impl Fn(&RtdTopic) -> XllResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.validator = Box::new(validator);
        self
    }

    /// Selects the final cell behavior when the shared producer fails.
    #[must_use]
    pub fn with_error_policy(mut self, policy: RtdProducerErrorPolicy) -> Self {
        self.error_policy = policy;
        self
    }

    /// Returns the data buffering policy.
    #[must_use]
    pub const fn delivery(&self) -> RtdDelivery {
        self.delivery
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

    fn start(
        &self,
        topic: &RtdTopic,
        mut channel: RtdChannelSubscription,
        budget: &Arc<ChannelBudget>,
    ) -> XllResult<RtdMultiplexedSubscription<T>> {
        let mut worker = self.worker.lock();
        if self.shared.closed.load(Ordering::Acquire) || worker.joining || worker.result.is_some() {
            return Err(XllError::Closing);
        }
        // Reserve before registering control events: a rejected first worker
        // must return the source slot even when no consumer exists yet.
        let permit = if worker.handle.is_none() {
            Some(ChannelBudget::try_worker(budget)?)
        } else {
            None
        };
        let id = self.shared.register(topic, channel.sender())?;
        let subscription_permit = channel.take_subscription_permit();
        let subscription = RtdMultiplexedSubscription {
            channel,
            shared: Arc::clone(&self.shared),
            id,
            permit: Some(subscription_permit),
        };
        if worker.handle.is_some() {
            return Ok(subscription);
        }
        let producer = worker.producer.take().ok_or(XllError::Closing)?;
        // Retain ownership outside the spawn closure. Failed OS thread creation
        // drops that closure synchronously; it must not destroy user captures
        // while this source's worker mutex is held.
        let pending = Arc::new(Mutex::new(Some(producer)));
        let thread_pending = Arc::clone(&pending);
        let shared = Arc::clone(&self.shared);
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
                worker.handle = Some(handle);
                worker.permit = permit;
                Ok(subscription)
            }
            Err(error) => {
                worker.producer = pending.lock().take();
                self.shared.stop();
                Err(XllError::Native {
                    code: error.raw_os_error().unwrap_or(0),
                    message: format!("failed to start RTD source worker: {error}"),
                })
            }
        }
    }
}

impl<T> RtdMultiplexedSource<T> {
    fn finish(&self) -> XllResult<()> {
        self.shared.stop();
        let (handle, producer, permit) = {
            let mut worker = self.worker.lock();
            while worker.joining {
                self.joined.wait(&mut worker);
            }
            if let Some(result) = &worker.result {
                return result.clone();
            }
            worker.joining = true;
            (
                worker.handle.take(),
                worker.producer.take(),
                worker.permit.take(),
            )
        };
        // No registry or worker lock is held while user captures are destroyed
        // or the shared job is joined. Keep the worker reservation through both.
        let dropped = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| drop(producer)))
            .map_err(|_| XllError::Panic);
        let joined = handle.map_or(Ok(()), |handle| {
            crate::panic_boundary::contain_panic(handle.join()).map_err(|_| XllError::Panic)
        });
        drop(permit);
        let result = dropped.and(joined);
        let mut worker = self.worker.lock();
        worker.result = Some(result.clone());
        worker.joining = false;
        self.joined.notify_all();
        result
    }
}

impl<T> Drop for RtdMultiplexedSource<T> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Framework-owned per-topic revocation barrier for a multiplexed source.
///
/// Disconnect never waits for upstream I/O or joins the source worker. It
/// closes the data queue, waits for in-flight publication, revokes the sink,
/// and transfers a pre-reserved removal event to the shared producer.
pub struct RtdMultiplexedSubscription<T> {
    channel: RtdChannelSubscription,
    shared: Arc<Shared<T>>,
    id: RtdSubscriptionId,
    permit: Option<SubscriptionPermit>,
}

impl<T> RtdMultiplexedSubscription<T> {
    fn finish(&mut self) -> XllResult<()> {
        let result = self.channel.finish();
        if let Some(permit) = self.permit.take() {
            self.shared.unregister(self.id, permit);
        }
        result
    }
}

impl<T> Drop for RtdMultiplexedSubscription<T> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

// SAFETY: only the channel's shared publisher can access a sink. finish and
// Drop revoke that channel and wait for publication before any exit. The
// producer and any escaped sender clones retain no usable runtime capability.
unsafe impl<T: Send + 'static> RtdSubscription for RtdMultiplexedSubscription<T> {
    fn request_cancel(&self) {
        self.channel.request_cancel();
    }
    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.finish()
    }
}

// SAFETY: an attached channel is locally owned until registration succeeds;
// rollback and unwinding revoke its sink through Drop. The shared producer
// receives only safe senders. Every successful subscription owns its channel
// revocation barrier; source shutdown additionally joins the shared job.
unsafe impl<T: IntoRtdValue + Send + 'static> RtdSource for RtdMultiplexedSource<T> {
    type Value = T;
    type Subscription = RtdMultiplexedSubscription<T>;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<T>) -> XllResult<Self::Subscription> {
        let runtime_budget = sink.channel_budget();
        let channel = RtdChannelSubscription::attach(
            self.delivery,
            Arc::clone(&self.budget),
            Arc::clone(&runtime_budget),
            sink,
        )?;
        (self.validator)(topic)?;
        self.start(topic, channel, &runtime_budget)
    }

    fn request_stop(&self) {
        self.shared.stop();
    }
    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.finish()
    }
}

#[cfg(test)]
mod tests;
