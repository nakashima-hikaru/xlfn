//! Production pending-task scheduling and calculation-cancellation fixture.
use super::{AsyncRuntime, BuiltinAsyncExecutor, BuiltinExecutorConfig, CalculationEpoch};
use crate::cancellation::{CancellationGuarantee, CancellationSource};
use crate::{AsyncPollerCount, AsyncTaskLimit};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

static POLLS: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicUsize = AtomicUsize::new(0);
const PENDING_POLLS: usize = 4;

struct WarmState {
    released: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

struct WarmFuture {
    state: Arc<WarmState>,
    registered: bool,
}

impl Future for WarmFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.state.released.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if !self.registered {
            self.state
                .wakers
                .lock()
                .unwrap()
                .push(context.waker().clone());
            self.registered = true;
        }
        Poll::Pending
    }
}

struct PollingFuture {
    remaining: usize,
    cancel: bool,
}

impl Future for PollingFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        POLLS.fetch_add(1, Ordering::Release);
        if self.remaining == 0 {
            return if self.cancel {
                Poll::Pending
            } else {
                Poll::Ready(())
            };
        }
        self.remaining -= 1;
        if !self.cancel || self.remaining != 0 {
            context.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

impl Drop for PollingFuture {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Release);
    }
}

/// One-poller fixture for allocation probes of rescheduling and cancellation.
/// Create and run these fixtures sequentially; observations use process statics.
pub struct AsyncPendingTaskBenchmark {
    runtime: AsyncRuntime<BuiltinAsyncExecutor>,
}

impl AsyncPendingTaskBenchmark {
    /// Starts and warms the pool, task shards, and cancellation slot high-water
    /// mark. Construction belongs outside the allocation measurement window.
    pub fn new() -> Self {
        let runtime = AsyncRuntime::new();
        runtime
            .start(
                BuiltinAsyncExecutor::new(
                    BuiltinExecutorConfig::new()
                        .with_poller_count(AsyncPollerCount::new(1).unwrap()),
                ),
                AsyncTaskLimit::DEFAULT,
            )
            .expect("pending probe executor startup");
        let slots = (0..64)
            .map(|_| CancellationSource::new(CancellationGuarantee::BestEffort).0)
            .collect::<Vec<_>>();
        drop(slots);
        let warm = Arc::new(WarmState {
            released: AtomicBool::new(false),
            wakers: Mutex::new(Vec::with_capacity(32)),
        });
        for _ in 0..32 {
            let (source, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
            runtime
                .submit(
                    CalculationEpoch::INITIAL,
                    WarmFuture {
                        state: Arc::clone(&warm),
                        registered: false,
                    },
                    source,
                )
                .expect("concurrent pending warmup admission");
        }
        while warm.wakers.lock().unwrap().len() != 32 {
            std::thread::yield_now();
        }
        warm.released.store(true, Ordering::Release);
        let wakers = std::mem::take(&mut *warm.wakers.lock().unwrap());
        for waker in wakers {
            waker.wake();
        }
        assert!(runtime.wait_idle());
        for _ in 0..256 {
            let (source, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
            runtime
                .submit(CalculationEpoch::INITIAL, std::future::ready(()), source)
                .expect("warm task admission");
            assert!(runtime.wait_idle());
        }
        Self { runtime }
    }

    /// Runs a batch with four pending polls per task. The ordinary case then
    /// completes; cancellation closes the calculation after every task stalls.
    /// The latter consumes this fixture's one calculation and runs once.
    pub fn run(&self, count: usize, cancel: bool) -> (usize, usize) {
        assert!((1..=32).contains(&count));
        let before_polls = POLLS.load(Ordering::Acquire);
        let before_drops = DROPS.load(Ordering::Acquire);
        for _ in 0..count {
            let (source, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
            self.runtime
                .submit(
                    CalculationEpoch::INITIAL,
                    PollingFuture {
                        remaining: PENDING_POLLS,
                        cancel,
                    },
                    source,
                )
                .expect("pending task admission");
        }
        if cancel {
            while POLLS.load(Ordering::Acquire) < before_polls + PENDING_POLLS * count {
                std::thread::yield_now();
            }
            self.runtime.cancel_current_calculation();
        }
        assert!(self.runtime.wait_idle());
        let polls = POLLS.load(Ordering::Acquire) - before_polls;
        let drops = DROPS.load(Ordering::Acquire) - before_drops;
        assert_eq!(polls, count * (PENDING_POLLS + usize::from(!cancel)));
        assert_eq!(drops, count);
        (polls, drops)
    }
}

impl Default for AsyncPendingTaskBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AsyncPendingTaskBenchmark {
    fn drop(&mut self) {
        assert!(self.runtime.close().unwrap().issues.is_empty());
    }
}
