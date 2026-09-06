#![allow(
    unsafe_code,
    reason = "The channel adapter owns and joins the only worker allowed to use an RTD sink"
)]

use super::source::{RtdSink, RtdSource, RtdSubscription};
use super::topic::RtdTopic;
use super::value::{IntoRtdValue, StoredRtdValue};
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};
use std::time::Duration;

type Producer<T> = dyn Fn(RtdTopic, RtdSender<T>) -> XllResult<()> + Send + Sync;

/// A safe RTD source with a bounded queue and framework-owned workers.
///
/// Each subscription runs the producer on its own thread. The producer sees
/// only an [`RtdSender`]; a separate publisher owns the non-owning [`RtdSink`].
/// Disconnect closes the queue and joins both workers, including when the
/// producer fails or panics. Sender clones may outlive the subscription: they
/// retain only a closed queue and cannot access the RTD runtime.
///
/// The producer must return when its sender closes. Use bounded I/O and
/// [`RtdSender::wait_closed`] for cancellation-aware polling. A producer that
/// never returns delays disconnect and add-in unload. Join any additional
/// threads or callbacks before returning from the producer.
pub struct RtdChannelSource<T> {
    capacity: NonZeroUsize,
    producer: Arc<Producer<T>>,
}

impl<T: IntoRtdValue + Send + 'static> RtdChannelSource<T> {
    /// Creates a source with a per-subscription queue capacity.
    ///
    /// `producer` runs asynchronously after subscription starts. Its error or
    /// panic closes sender admission and is reported during disconnect. A
    /// successful return drains accepted values, then closes the publisher.
    pub fn new(
        capacity: NonZeroUsize,
        producer: impl Fn(RtdTopic, RtdSender<T>) -> XllResult<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            capacity,
            producer: Arc::new(producer),
        }
    }
}

/// A cloneable, owning sender that never contains a raw RTD capability.
///
/// Values are converted and validated on the calling thread, before the queue
/// is locked. Successful enqueueing does not guarantee delivery: disconnect
/// discards pending values, and an RTD publication error closes the channel.
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
        !self.channel.state.lock().accepting
    }

    /// Waits until admission closes or `timeout` elapses. Returns `true` when
    /// closed, so a polling producer can exit promptly during disconnect.
    pub fn wait_closed(&self, timeout: Duration) -> bool {
        let mut state = self.channel.state.lock();
        self.channel
            .changed
            .wait_while_for(&mut state, |state| state.accepting, timeout);
        !state.accepting
    }
}

impl<T: IntoRtdValue> RtdSender<T> {
    /// Validates and enqueues a value without waiting for queue capacity.
    ///
    /// Returns [`XllError::Overloaded`] when the bounded queue is full, or
    /// [`XllError::Closing`] after admission closes. Value conversion can
    /// return its own validation error. A concurrent close is rechecked after
    /// conversion; user conversion code never runs with the queue locked.
    pub fn try_send(&self, value: T) -> XllResult<()> {
        if self.is_closed() {
            return Err(XllError::Closing);
        }
        let value = value.into_rtd_value()?.into_stored()?;
        {
            let mut state = self.channel.state.lock();
            if !state.accepting {
                return Err(XllError::Closing);
            }
            if state.values.len() >= self.channel.capacity.get() {
                return Err(XllError::Overloaded);
            }
            state.values.push_back(value);
        }
        self.channel.changed.notify_all();
        Ok(())
    }
}

struct ChannelState {
    values: VecDeque<StoredRtdValue>,
    accepting: bool,
    stopping: bool,
}

struct Channel {
    capacity: NonZeroUsize,
    state: Mutex<ChannelState>,
    changed: Condvar,
}

impl Channel {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            state: Mutex::new(ChannelState {
                values: VecDeque::new(),
                accepting: true,
                stopping: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn sender<T>(self: &Arc<Self>) -> RtdSender<T> {
        RtdSender {
            channel: Arc::clone(self),
            _value: PhantomData,
        }
    }

    fn producer_finished(&self) {
        self.state.lock().accepting = false;
        self.changed.notify_all();
    }

    fn close(&self) {
        let pending = {
            let mut state = self.state.lock();
            state.accepting = false;
            state.stopping = true;
            std::mem::take(&mut state.values)
        };
        self.changed.notify_all();
        drop(pending);
    }

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
/// also closes admission and joins both workers; a failed subscription setup
/// cannot detach the publisher and leave a sink behind.
pub struct RtdChannelSubscription {
    channel: Arc<Channel>,
    publisher: Option<JoinHandle<XllResult<()>>>,
    producer: Option<JoinHandle<XllResult<()>>>,
}

impl RtdChannelSubscription {
    fn start_publisher<T: IntoRtdValue + Send + 'static>(
        channel: Arc<Channel>,
        sink: RtdSink<T>,
    ) -> XllResult<Self> {
        let mut subscription = Self {
            channel,
            publisher: None,
            producer: None,
        };
        let publisher_channel = Arc::clone(&subscription.channel);
        subscription.publisher = Some(
            Builder::new()
                .name("xlfn-rtd-publisher".into())
                .spawn(move || {
                    let _close = scopeguard::guard((), |_| publisher_channel.close());
                    while let Some(value) = publisher_channel.receive() {
                        // Values have already been converted outside framework
                        // locks. Only this worker can reach the erased sink.
                        sink.publish_stored(value)?;
                    }
                    Ok(())
                })
                .map_err(spawn_error)?,
        );
        Ok(subscription)
    }

    fn finish(&mut self) -> XllResult<()> {
        self.channel.close();
        // Join both before reporting errors. Consume panic payloads at once so
        // their arbitrary destructors cannot interrupt another worker's join
        // or unwind from this subscription's Drop.
        let publisher = self
            .publisher
            .take()
            .map(|worker| crate::panic_boundary::contain_panic(worker.join()));
        let producer = self
            .producer
            .take()
            .map(|worker| crate::panic_boundary::contain_panic(worker.join()));
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

// SAFETY: only the publisher receives a sink. Closing admission wakes both
// workers, and every shutdown path joins the publisher before returning. Any
// surviving sender owns only the channel, which is independent of the runtime.
unsafe impl RtdSubscription for RtdChannelSubscription {
    fn request_cancel(&self) {
        self.channel.close();
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.finish()
    }
}

// SAFETY: the local subscription is a join-on-drop guard before any worker is
// spawned. Failure/unwind in setup therefore joins an already-started
// publisher. On success this same guard becomes the returned subscription.
// User code receives no sink, and neither worker can detach its join handle.
unsafe impl<T: IntoRtdValue + Send + 'static> RtdSource for RtdChannelSource<T> {
    type Value = T;
    type Subscription = RtdChannelSubscription;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<T>) -> XllResult<Self::Subscription> {
        let channel = Arc::new(Channel::new(self.capacity));
        let mut subscription = RtdChannelSubscription::start_publisher(Arc::clone(&channel), sink)?;
        let producer = Arc::clone(&self.producer);
        let topic = topic.clone();
        subscription.producer = Some(
            Builder::new()
                .name("xlfn-rtd-producer".into())
                .spawn(move || {
                    let _finish = scopeguard::guard((), |_| channel.producer_finished());
                    producer(topic, channel.sender())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::{
        RefreshOutcome, RtdValue, SubscriptionRuntime, SubscriptionServerHandle, TopicId,
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

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
            .prepare(&source, RtdTopic::single("channel-test").unwrap())
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

    #[test]
    fn bounded_channel_validates_before_admission() {
        let channel = Arc::new(Channel::new(capacity()));
        let sender = channel.sender::<f64>();
        assert!(sender.try_send(f64::NAN).is_err());
        assert!(channel.state.lock().values.is_empty());
        sender.try_send(1.0).unwrap();
        sender.try_send(2.0).unwrap();
        assert!(matches!(sender.try_send(3.0), Err(XllError::Overloaded)));
        assert_eq!(channel.receive(), Some(StoredRtdValue::Number(1.0)));
        assert_eq!(channel.receive(), Some(StoredRtdValue::Number(2.0)));
        channel.close();
        assert!(sender.is_closed());
        assert!(matches!(sender.try_send(3.0), Err(XllError::Closing)));
        assert_eq!(channel.receive(), None);
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
        let sender = channel.sender::<CloseDuringConversion>();
        assert!(matches!(
            sender.try_send(CloseDuringConversion(Arc::clone(&channel))),
            Err(XllError::Closing)
        ));
        assert!(channel.state.lock().values.is_empty());
    }

    #[test]
    fn completed_producer_drains_values_then_stops_publisher() {
        let (_runtime, server, sink) = sink();
        let source = RtdChannelSource::new(capacity(), |_, sender| {
            sender.try_send(17)?;
            sender.try_send(42)
        });
        let mut subscription = source
            .subscribe(&RtdTopic::single("finite").unwrap(), sink)
            .unwrap();
        subscription
            .publisher
            .take()
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(42));
        batch.complete(RefreshOutcome::Delivered).unwrap();
        Box::new(subscription).disconnect_and_wait().unwrap();
    }

    #[test]
    fn disconnect_joins_producer_and_closes_escaped_senders() {
        let (sender_tx, sender_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let finished = Arc::new(AtomicBool::new(false));
        let producer_finished = Arc::clone(&finished);
        let source = RtdChannelSource::new(capacity(), move |_, sender| {
            sender_tx.send(sender.clone()).unwrap();
            assert!(sender.wait_closed(DEADLINE));
            closed_tx.send(()).unwrap();
            release_rx.lock().recv_timeout(DEADLINE).unwrap();
            producer_finished.store(true, Ordering::Release);
            Ok(())
        });
        let (arena, source) = crate::subscription::SourceArena::with_source(
            crate::generation::RuntimeGeneration::new(1).unwrap(),
            source,
        )
        .unwrap();
        let runtime = Arc::new(SubscriptionRuntime::with_sources_for_internal(arena));
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(&source, RtdTopic::single("disconnect").unwrap())
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
        assert!(matches!(sender.try_send(1), Err(XllError::Closing)));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(DEADLINE).unwrap().unwrap();
        disconnect.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
        drop(runtime);
        assert!(sender_clone.is_closed());
        assert!(matches!(sender_clone.try_send(2), Err(XllError::Closing)));
    }

    #[test]
    fn disconnect_waits_for_in_flight_publisher() {
        let (_runtime, server, sink) = sink();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let notifier = Arc::new(crate::rtd::test_support::TestNotifierState::new());
        *notifier.entered.lock() = Some(entered_tx);
        *notifier.release.lock() = Some(release_rx);
        server
            .attach_update_notifier(crate::excel_rtd::RtdNotifier::for_test(notifier))
            .unwrap();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_, sender| {
            sender_tx.send(sender.clone()).unwrap();
            sender.try_send(1)?;
            assert!(sender.wait_closed(DEADLINE));
            Ok(())
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
        assert!(matches!(sender.try_send(2), Err(XllError::Closing)));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(DEADLINE).unwrap().unwrap();
        disconnect.join().unwrap();
    }

    #[test]
    fn panic_in_producer_closes_sender_and_is_reported_after_join() {
        let (_runtime, _server, sink) = sink();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_, sender| {
            sender_tx.send(sender).unwrap();
            panic!("injected producer panic");
        });
        let subscription = source
            .subscribe(&RtdTopic::single("panic").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert!(sender.wait_closed(DEADLINE));
        assert!(matches!(
            Box::new(subscription).disconnect_and_wait(),
            Err(XllError::Panic)
        ));
        assert!(matches!(sender.try_send(1), Err(XllError::Closing)));
    }

    #[test]
    fn producer_error_closes_sender_and_is_reported_after_join() {
        let (_runtime, _server, sink) = sink();
        let (sender_tx, sender_rx) = mpsc::channel();
        let source = RtdChannelSource::new(capacity(), move |_, sender| {
            sender_tx.send(sender).unwrap();
            Err(XllError::Overloaded)
        });
        let subscription = source
            .subscribe(&RtdTopic::single("error").unwrap(), sink)
            .unwrap();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert!(sender.wait_closed(DEADLINE));
        assert!(matches!(
            Box::new(subscription).disconnect_and_wait(),
            Err(XllError::Overloaded)
        ));
    }

    #[test]
    fn custom_producer_panic_payload_cannot_unwind_disconnect_or_drop() {
        for explicit_disconnect in [false, true] {
            let (_runtime, _server, sink) = sink();
            let payload_drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let producer_drops = Arc::clone(&payload_drops);
            let source = RtdChannelSource::new(capacity(), move |_, _| {
                std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                    &producer_drops,
                )));
            });
            let subscription = source
                .subscribe(&RtdTopic::single("custom-payload").unwrap(), sink)
                .unwrap();
            if explicit_disconnect {
                assert!(matches!(
                    Box::new(subscription).disconnect_and_wait(),
                    Err(XllError::Panic)
                ));
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
        let source = RtdChannelSource::new(capacity(), move |_, sender| {
            sender_tx.send(sender.clone()).unwrap();
            assert!(sender.wait_closed(DEADLINE));
            producer_finished.store(true, Ordering::Release);
            Ok(())
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
        assert!(matches!(sender.try_send(1), Err(XllError::Closing)));
    }

    #[test]
    fn publisher_only_setup_error_and_unwind_join_before_return() {
        for unwind in [false, true] {
            let (runtime, _server, sink) = sink();
            let channel = Arc::new(Channel::new(capacity()));
            let sender = channel.sender::<i32>();
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
            assert_eq!(Arc::strong_count(&sender.channel), 1);
            drop(runtime);
            assert!(matches!(sender.try_send(1), Err(XllError::Closing)));
        }
    }
}
