use super::super::budget::RtdLatest;
use super::*;
use crate::subscription::{RtdCapacity, RtdLimits, RtdValue};
use std::sync::Arc as StdArc;
use std::sync::atomic::AtomicUsize;

fn bounded(value: usize) -> RtdCapacity {
    RtdCapacity::disabled_if_zero(value)
}

fn fifo(capacity: usize) -> Arc<Channel> {
    Arc::new(Channel::new(NonZeroUsize::new(capacity).unwrap()))
}

#[test]
fn miri_batch_send_recovers_the_whole_fifo_without_converting_again() {
    struct Input {
        conversions: StdArc<AtomicUsize>,
        value: i32,
    }
    impl IntoRtdValue for Input {
        fn into_rtd_value(self) -> XllResult<RtdValue> {
            self.conversions.fetch_add(1, Ordering::Relaxed);
            Ok(RtdValue::Integer(self.value))
        }
    }
    let conversions = StdArc::new(AtomicUsize::new(0));
    let input = |value| Input {
        conversions: StdArc::clone(&conversions),
        value,
    };
    let channel = fifo(3);
    let sender = Channel::sender(&channel);
    sender.try_send(input(0)).unwrap();
    let error = sender
        .try_send_batch([input(1), input(2), input(3)])
        .unwrap_err();
    assert!(matches!(error, RtdBatchSendError::Full(_)));
    let pending = error.into_pending().unwrap();
    assert_eq!(pending.len(), 3);
    assert_eq!(channel.state.lock().values.len(), 1);
    assert_eq!(channel.receive(), Some(StoredRtdValue::Integer(0)));
    sender.try_send_pending_batch(pending).unwrap();
    for value in 1..=3 {
        assert_eq!(channel.receive(), Some(StoredRtdValue::Integer(value)));
    }
    channel.close();
    let pending = sender
        .try_send_batch([input(4), input(5)])
        .unwrap_err()
        .into_pending()
        .unwrap();
    assert_eq!(pending.len(), 2);
    let other = fifo(2);
    Channel::sender::<Input>(&other)
        .try_send_pending_batch(pending)
        .unwrap();
    assert_eq!(other.receive(), Some(StoredRtdValue::Integer(4)));
    assert_eq!(other.receive(), Some(StoredRtdValue::Integer(5)));
    assert_eq!(conversions.load(Ordering::Relaxed), 6);
}

#[test]
fn batch_conversion_and_length_errors_never_admit_a_prefix() {
    struct Input(bool);
    impl IntoRtdValue for Input {
        fn into_rtd_value(self) -> XllResult<RtdValue> {
            if self.0 {
                Ok(RtdValue::Integer(1))
            } else {
                Err(XllError::Closing)
            }
        }
    }
    let channel = fifo(32);
    let sender = Channel::sender(&channel);
    assert!(matches!(
        sender.try_send_batch([Input(true), Input(false), Input(true)]),
        Err(RtdBatchSendError::Invalid(XllError::Closing))
    ));
    assert!(channel.state.lock().values.is_empty());
    let integers = Channel::sender::<i32>(&channel);
    assert!(matches!(
        integers.try_send_batch(0..33),
        Err(RtdBatchSendError::Invalid(_))
    ));
    assert!(channel.state.lock().values.is_empty());
    integers.try_send_batch(0..32).unwrap();
    assert_eq!(channel.state.lock().values.len(), 32);
    channel.close();
    integers.try_send_batch([]).unwrap();
}

#[test]
fn miri_batch_budget_rejection_rolls_back_the_source_and_preserves_every_value() {
    let source = Arc::new(ChannelBudget::source(
        RtdSourceLimits::standard().with_max_buffered_values(bounded(4)),
    ));
    let runtime = Arc::new(ChannelBudget::runtime(
        RtdLimits::standard().with_max_buffered_values(bounded(3)),
    ));
    let channel = || {
        Arc::new(Channel::with_budgets(
            RtdFifo::new(NonZeroUsize::new(4).unwrap()),
            BudgetPair::new(Arc::clone(&source), Arc::clone(&runtime)),
        ))
    };
    let first = channel();
    let second = channel();
    let third = channel();
    Channel::sender::<i32>(&first)
        .try_send_batch([1, 2])
        .unwrap();
    let pending = Channel::sender::<i32>(&second)
        .try_send_batch([3, 4])
        .unwrap_err()
        .into_pending()
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(second.state.lock().values.is_empty());
    first.close();
    Channel::sender::<i32>(&second)
        .try_send_pending_batch(pending)
        .unwrap();
    // This fits only if the failed source-side reservation was rolled back.
    Channel::sender::<i32>(&third).try_send_batch([5]).unwrap();
    second.close();
    third.close();

    let bytes = value_bytes(&StoredRtdValue::String("x".into()));
    let limited = Arc::new(Channel::with_budgets(
        RtdFifo::new(NonZeroUsize::new(2).unwrap()),
        BudgetPair::new(
            Arc::new(ChannelBudget::source(
                RtdSourceLimits::standard().with_max_buffered_bytes(bounded(bytes * 2 - 1)),
            )),
            Arc::new(ChannelBudget::runtime(RtdLimits::standard())),
        ),
    ));
    let sender = Channel::sender::<String>(&limited);
    assert!(matches!(
        sender.try_send_batch(["x".into(), "y".into()]),
        Err(RtdBatchSendError::Full(_))
    ));
    assert!(limited.state.lock().values.is_empty());
    sender.try_send_batch(["z".into()]).unwrap();
    limited.close();
}

#[test]
fn batch_latest_coalesces_atomically_and_returns_its_full_input_on_rejection() {
    let channel = Arc::new(Channel::with_budgets(
        RtdLatest,
        BudgetPair::new(
            Arc::new(ChannelBudget::source(
                RtdSourceLimits::standard().with_max_buffered_bytes(bounded(value_bytes(
                    &StoredRtdValue::String("old".into()),
                ))),
            )),
            Arc::new(ChannelBudget::runtime(RtdLimits::standard())),
        ),
    ));
    let sender = Channel::sender_with_policy::<String, _>(&channel, RtdLatest);
    sender.try_send("old".into()).unwrap();
    sender
        .try_send_batch(["discarded-large-prefix".into(), "xy".into()])
        .unwrap();
    let pending = sender
        .try_send_batch(["a".into(), "too-long".into()])
        .unwrap_err()
        .into_pending()
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(channel.receive(), Some(StoredRtdValue::String("xy".into())));
    let other = fifo(2);
    Channel::sender::<String>(&other)
        .try_send_pending_batch(pending)
        .unwrap();
    assert_eq!(other.receive(), Some(StoredRtdValue::String("a".into())));
    assert_eq!(
        other.receive(),
        Some(StoredRtdValue::String("too-long".into()))
    );
}

#[test]
fn miri_pending_fifo_batch_can_be_retried_on_typed_latest_sender() {
    let fifo = fifo(1);
    let pending = Channel::sender::<i32>(&fifo)
        .try_send_batch([1, 2, 3])
        .unwrap_err()
        .into_pending()
        .unwrap();
    assert!(fifo.state.lock().values.is_empty());
    let latest = Arc::new(Channel::with_budgets(
        RtdLatest,
        BudgetPair::new(
            Arc::new(ChannelBudget::source(RtdSourceLimits::standard())),
            Arc::new(ChannelBudget::runtime(RtdLimits::standard())),
        ),
    ));
    let sender = Channel::sender_with_policy::<i32, _>(&latest, RtdLatest);
    sender.try_send(0).unwrap();
    sender.try_send_pending_batch(pending).unwrap();
    assert_eq!(latest.state.lock().values.len(), 1);
    assert_eq!(latest.receive(), Some(StoredRtdValue::Integer(3)));
    latest.close();
    fifo.close();
}

#[test]
fn batch_admission_wakes_an_unattached_blocking_consumer() {
    let channel = fifo(3);
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    let consumer = Arc::clone(&channel);
    let worker = std::thread::spawn(move || {
        let mut values = smallvec::SmallVec::new();
        let completed = consumer.receive_batch(&mut values);
        sent.send((completed, values)).unwrap();
    });
    Channel::sender::<i32>(&channel)
        .try_send_batch([1, 2, 3])
        .unwrap();
    let result = received.recv_timeout(Duration::from_secs(5));
    channel.close();
    worker.join().unwrap();
    let (completed, values) = result.unwrap();
    assert!(completed);
    assert_eq!(
        values.as_slice(),
        [
            StoredRtdValue::Integer(1),
            StoredRtdValue::Integer(2),
            StoredRtdValue::Integer(3)
        ]
    );
}

#[test]
fn miri_publisher_queue_warm_access_does_not_take_the_worker_lock() {
    let pool = PublisherPool::new();
    let queue = pool.queue().unwrap();
    std::thread::scope(|scope| {
        let guard = pool.worker.lock();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let shared = &pool;
        let worker = scope.spawn(move || sent.send(shared.queue()).unwrap());
        let result = received.recv_timeout(Duration::from_secs(5));
        drop(guard);
        worker.join().unwrap();
        assert!(Arc::ptr_eq(&queue, &result.unwrap().unwrap()));
    });
    pool.stop();
    assert!(matches!(pool.queue(), Err(XllError::Closing)));
}
