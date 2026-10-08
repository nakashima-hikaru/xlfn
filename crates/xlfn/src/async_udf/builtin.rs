//! Optional runnable pool; task lifecycle remains in the async runtime core.

mod queue;
mod worker;

use super::{AsyncExecutor, AsyncTask};
use crate::diagnostics::id::DiagnosticId;
use crate::error::DomainErrorCode;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use async_task::Runnable;
use crossbeam_utils::sync::Parker;
use queue::RunnableQueue;
use std::cell::Cell;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use xlfn_kernel::operation_gate::OperationGate;

thread_local! {
    static SCHEDULER_POOL: Cell<usize> = const { Cell::new(0) };
}

struct SchedulerScope(usize);

impl SchedulerScope {
    fn enter(shared: &PoolShared) -> Self {
        Self(SCHEDULER_POOL.replace(std::ptr::from_ref(shared).addr()))
    }
}

impl Drop for SchedulerScope {
    fn drop(&mut self) {
        SCHEDULER_POOL.set(self.0);
    }
}

fn is_current_scheduler(shared: &Arc<PoolShared>) -> bool {
    SCHEDULER_POOL.get() == Arc::as_ptr(shared).addr()
}

/// Bounded number of poller threads in the built-in async executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AsyncPollerCount(NonZeroUsize);

impl AsyncPollerCount {
    /// Maximum supported number of pollers in one pool.
    pub const MAX: usize = 32;
    /// Default pool size of four pollers.
    pub const DEFAULT: Self = Self(NonZeroUsize::new(4).expect("default poller count is non-zero"));

    /// Validates a poller count in `1..=MAX`.
    #[must_use]
    pub const fn new(poller_count: usize) -> Option<Self> {
        if poller_count == 0 || poller_count > Self::MAX {
            None
        } else {
            Some(Self(
                NonZeroUsize::new(poller_count).expect("poller count is non-zero"),
            ))
        }
    }

    /// Returns the validated poller count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for AsyncPollerCount {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<usize> for AsyncPollerCount {
    type Error = XllError;

    fn try_from(poller_count: usize) -> XllResult<Self> {
        Self::new(poller_count).ok_or(XllError::Domain {
            code: DomainErrorCode::InvalidInput,
        })
    }
}

/// Policy used when the built-in executor starts its own poller pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinExecutorConfig {
    poller_count: AsyncPollerCount,
}

impl BuiltinExecutorConfig {
    /// Creates a policy using [`AsyncPollerCount::DEFAULT`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            poller_count: AsyncPollerCount::DEFAULT,
        }
    }

    /// Sets the number of threads that poll submitted tasks.
    #[must_use]
    pub const fn with_poller_count(mut self, poller_count: AsyncPollerCount) -> Self {
        self.poller_count = poller_count;
        self
    }
}

impl Default for BuiltinExecutorConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// An idle, owned executor that starts its pool during add-in opening.
///
/// The executor owns runnable scheduling and poller joins. Task identity,
/// cancellation, calculation generations and Excel delivery belong to the
/// framework runtime. Creating this value does not spawn a thread.
pub struct BuiltinAsyncExecutor {
    config: BuiltinExecutorConfig,
    // Published once after successful startup. Reservations and scheduler
    // callbacks own Arc copies, independent from runtime publication.
    shared: OnceLock<Arc<PoolShared>>,
    lifecycle: Mutex<Lifecycle>,
    stopped: Condvar,
    #[cfg(test)]
    fail_start_at: Option<usize>,
}

enum Lifecycle {
    Idle,
    Running(Pool),
    Closing,
    Stopped(XllResult<()>),
}

/// Owned scheduling state reserved before task publication.
#[doc(hidden)]
pub struct BuiltinReservation {
    shared: Arc<PoolShared>,
}

impl BuiltinAsyncExecutor {
    /// Creates an idle executor with the supplied poller policy.
    #[must_use]
    pub const fn new(config: BuiltinExecutorConfig) -> Self {
        Self {
            config,
            shared: OnceLock::new(),
            lifecycle: Mutex::new(Lifecycle::Idle),
            stopped: Condvar::new(),
            #[cfg(test)]
            fail_start_at: None,
        }
    }

    #[cfg(test)]
    fn with_start_failure(config: BuiltinExecutorConfig, fail_at: usize) -> Self {
        let mut executor = Self::new(config);
        executor.fail_start_at = Some(fail_at);
        executor
    }
}

impl AsyncExecutor for BuiltinAsyncExecutor {
    type Reservation = BuiltinReservation;

    fn start(&self) -> XllResult<()> {
        let mut lifecycle = self.lifecycle.lock();
        match &*lifecycle {
            Lifecycle::Running(pool) => {
                return if pool.shared.failed.load(Ordering::Acquire) {
                    Err(pool_failure())
                } else {
                    Ok(())
                };
            }
            Lifecycle::Closing | Lifecycle::Stopped(_) => return Err(XllError::Closing),
            Lifecycle::Idle => {}
        }
        #[cfg(test)]
        let fail_at = self.fail_start_at;
        #[cfg(not(test))]
        let fail_at = None;
        let pool = Pool::start(self.config.poller_count.get(), fail_at)?;
        if self.shared.set(Arc::clone(&pool.shared)).is_err() {
            xlfn_kernel::invariant::fail_stop();
        }
        *lifecycle = Lifecycle::Running(pool);
        Ok(())
    }

    fn reserve(&self) -> XllResult<Self::Reservation> {
        let shared = self.shared.get().ok_or(XllError::Closing)?;
        if shared.closing.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        if shared.failed.load(Ordering::Acquire) || shared.live_pollers.load(Ordering::Acquire) == 0
        {
            return Err(pool_failure());
        }
        Ok(BuiltinReservation {
            shared: Arc::clone(shared),
        })
    }

    fn submit<F>(&self, reservation: Self::Reservation, task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let shared = reservation.shared;
        let schedule = move |runnable| shared.schedule(runnable);
        let (runnable, task) = async_task::spawn(task, schedule);
        task.detach();
        runnable.schedule();
    }

    fn shutdown(&self) -> XllResult<()> {
        if self
            .shared
            .get()
            .is_some_and(|shared| worker::is_current_poller(shared) || is_current_scheduler(shared))
        {
            return Err(pool_failure());
        }
        let mut pool = {
            let mut lifecycle = self.lifecycle.lock();
            loop {
                match &*lifecycle {
                    Lifecycle::Idle => {
                        *lifecycle = Lifecycle::Stopped(Ok(()));
                        self.stopped.notify_all();
                        return Ok(());
                    }
                    Lifecycle::Stopped(result) => return result.clone(),
                    Lifecycle::Closing => self.stopped.wait(&mut lifecycle),
                    Lifecycle::Running(pool) => {
                        pool.shared.closing.store(true, Ordering::Release);
                        let Lifecycle::Running(pool) =
                            std::mem::replace(&mut *lifecycle, Lifecycle::Closing)
                        else {
                            xlfn_kernel::invariant::fail_stop();
                        };
                        break pool;
                    }
                }
            }
        };
        let result = pool.close_and_join();
        *self.lifecycle.lock() = Lifecycle::Stopped(result.clone());
        self.stopped.notify_all();
        result
    }
}

impl Drop for BuiltinAsyncExecutor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Pool {
    shared: Arc<PoolShared>,
    pollers: Vec<JoinHandle<()>>,
}

impl Pool {
    fn start(poller_count: usize, fail_at: Option<usize>) -> XllResult<Self> {
        let mut local_pollers = Vec::with_capacity(poller_count);
        let mut stealers = Vec::with_capacity(poller_count);
        let mut unparkers = Vec::with_capacity(poller_count);
        for _ in 0..poller_count {
            let local = crossbeam_deque::Worker::new_fifo();
            stealers.push(local.stealer());
            let parker = Parker::new();
            unparkers.push(parker.unparker().clone());
            local_pollers.push((local, parker));
        }
        let shared = Arc::new(PoolShared {
            queue: RunnableQueue::new(stealers.into_boxed_slice(), unparkers.into_boxed_slice()),
            scheduler_callbacks: OperationGate::new(),
            live_pollers: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            #[cfg(test)]
            before_poll_hook: Mutex::new(None),
            #[cfg(test)]
            after_scheduler_drop_hook: Mutex::new(None),
        });
        let mut pool = Self {
            shared,
            pollers: Vec::with_capacity(poller_count),
        };
        for (index, (local, parker)) in local_pollers.into_iter().enumerate() {
            if fail_at == Some(index) {
                return Err(pool_failure());
            }
            pool.shared.live_pollers.fetch_add(1, Ordering::Relaxed);
            let shared = Arc::clone(&pool.shared);
            let spawned = thread::Builder::new()
                .name(format!("xlfn-async-poller-{index}"))
                .spawn(move || worker::run_poller(index, shared, local, parker));
            let handle = match spawned {
                Ok(handle) => handle,
                Err(_) => {
                    let _ = xlfn_kernel::invariant::checked_atomic_dec_release(
                        &pool.shared.live_pollers,
                    );
                    return Err(pool_failure());
                }
            };
            pool.pollers.push(handle);
        }
        Ok(pool)
    }

    fn close_and_join(&mut self) -> XllResult<()> {
        self.shared.closing.store(true, Ordering::Release);
        let self_callback = is_current_scheduler(&self.shared);
        self.shared.scheduler_callbacks.begin_close();
        if !self_callback {
            self.shared
                .scheduler_callbacks
                .close_and_wait_begin()
                .wait();
        }
        self.shared.queue.seal_and_wake_all();
        // Drop may run inside a rejected runnable's user destructor. Its
        // scheduler closure retains this shared allocation through callback
        // release; waiting for our own admission here would deadlock.
        let mut result = if self_callback {
            Err(pool_failure())
        } else {
            Ok(())
        };
        for poller in self.pollers.drain(..) {
            if poller.thread().id() == thread::current().id() {
                // A final executor owner may be dropped by its own task. The
                // poller retains the shared allocation until its sealed loop
                // exits; direct shutdown rejects this self-join above.
                result = Err(pool_failure());
                continue;
            }
            if crate::panic_boundary::contain_panic(poller.join()).is_err() {
                result = Err(XllError::Panic);
            }
        }
        result
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let _ = self.close_and_join();
    }
}

struct PoolShared {
    queue: RunnableQueue,
    scheduler_callbacks: OperationGate,
    live_pollers: AtomicUsize,
    failed: AtomicBool,
    closing: AtomicBool,
    #[cfg(test)]
    before_poll_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_scheduler_drop_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl PoolShared {
    fn schedule(&self, runnable: Runnable) {
        let Ok(_callback) = self.scheduler_callbacks.enter() else {
            drop_runnable(runnable);
            return;
        };
        let _scope = SchedulerScope::enter(self);
        if self.live_pollers.load(Ordering::Acquire) == 0 {
            drop_runnable(runnable);
            #[cfg(test)]
            if let Some(hook) = self.after_scheduler_drop_hook.lock().clone() {
                hook();
            }
        } else {
            self.queue.schedule(runnable);
        }
    }
}

fn pool_failure() -> XllError {
    XllError::Internal {
        diagnostic_id: DiagnosticId::ASYNC_SPAWN,
    }
}

fn drop_runnable(runnable: Runnable) {
    let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
        drop(runnable);
    }));
}

#[cfg(test)]
mod tests;
