#![cfg(feature = "rtd")]

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use xlfn::rtd::{
    RtdCapacity, RtdDelivery, RtdLimits, RtdMultiplexedSource, RtdProducerErrorPolicy, RtdSource,
    RtdSourceContext, RtdSourceEvent, RtdSourceLimits,
};
use xlfn::{ExcelError, XllError, XllResult};

#[test]
fn multiplexed_source_accepts_an_owned_non_sync_job_without_starting_it() {
    fn accepts_source<S: RtdSource<Value = i32>>(_: &S) {}

    let started = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&started);
    // Cell makes the captured job non-Sync; each source runs its job once.
    let state = Cell::new(0);
    let source = RtdMultiplexedSource::new(RtdDelivery::Latest, move |context| {
        observed.store(true, Ordering::Release);
        state.set(state.get() + 1);
        serve(context, state.get())
    })
    .with_topic_validator(|topic| {
        if topic.part(0) == Some("metric") {
            Ok(())
        } else {
            Err(XllError::Overloaded)
        }
    })
    .with_limits(
        RtdSourceLimits::standard()
            .with_max_subscriptions(RtdCapacity::bounded(NonZeroUsize::new(8).unwrap()))
            .with_max_buffered_values(RtdCapacity::disabled_if_zero(16))
            .with_max_buffered_bytes(RtdCapacity::disabled_if_zero(4096)),
    )
    .with_error_policy(RtdProducerErrorPolicy::PublishError(
        ExcelError::NotAvailable,
    ));
    accepts_source(&source);
    drop(source);
    assert!(!started.load(Ordering::Acquire));
}

// Compiles the consumer side of the public control protocol without exposing
// test-only constructors or a raw publication capability.
fn serve(context: RtdSourceContext<i32>, initial: i32) -> XllResult<()> {
    while !context.is_closed() {
        match context.recv_timeout(Duration::from_millis(1)) {
            Ok(Some(RtdSourceEvent::Subscribe { id, topic, sender })) => {
                let _identity = id;
                assert!(!topic.is_empty());
                sender
                    .try_send(initial)
                    .map_err(|error| error.into_error())?;
            }
            Ok(Some(RtdSourceEvent::Unsubscribe { id })) => {
                let _identity = id;
            }
            Ok(_) => {}
            Err(XllError::Closing) if context.is_closed() => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[test]
fn adapter_buffer_and_worker_limits_are_distinct_from_refresh_capacity() {
    const SOURCE: RtdSourceLimits = RtdSourceLimits::standard()
        .with_max_subscriptions(RtdCapacity::disabled_if_zero(2))
        .with_max_buffered_values(RtdCapacity::disabled_if_zero(3))
        .with_max_buffered_bytes(RtdCapacity::disabled_if_zero(512));
    const RUNTIME: RtdLimits = RtdLimits::standard()
        .with_max_workers(RtdCapacity::disabled_if_zero(1))
        .with_max_buffered_values(RtdCapacity::disabled_if_zero(7))
        .with_max_buffered_bytes(RtdCapacity::disabled());

    assert_eq!(SOURCE.max_subscriptions().get(), 2);
    assert_eq!(SOURCE.max_buffered_values().get(), 3);
    assert_eq!(SOURCE.max_buffered_bytes().get(), 512);
    assert_eq!(RUNTIME.max_workers().get(), 1);
    assert_eq!(RUNTIME.max_buffered_values().get(), 7);
    assert!(RUNTIME.max_buffered_bytes().is_disabled());
    assert_eq!(
        RUNTIME.max_queued_updates(),
        RtdLimits::standard().max_queued_updates()
    );
}
