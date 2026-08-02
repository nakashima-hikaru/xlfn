#![allow(unsafe_code)]
#![cfg(feature = "async")]

use crate::cancellation::CancellationSource;
use crate::return_value::AsyncReturnPointer;
use crate::{
    CallId, CallMetadata, CallOutcome, CancellationGuarantee, CancellationToken, IntoExcelValue,
    Runtime, UdfResultKind, XllError, XllResult,
};
use async_channel::{Receiver, Sender};
use async_task::Runnable;
use futures_util::FutureExt;
use futures_util::future::{AbortHandle, Abortable};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Duration;
use std::time::{Instant, SystemTime};
use xlfn_sys::{
    XL_ASYNC_RETURN, XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value,
    XLRET_SUCCESS, XLTYPE_BIG_DATA, XLTYPE_BOOL,
};

const MAX_PENDING: usize = 4096;
const MAX_ASYNC_HANDLE_BYTES: usize = 1024 * 1024;
pub(crate) struct AsyncManager {
    state: Mutex<ExecutorState>,
    state_changed: Condvar,
    current_generation: AtomicU64,
    #[cfg(test)]
    after_generation_publish_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

enum ExecutorState {
    Stopped,
    Running(Executor),
    // `None` means one close caller owns the executor while it waits without
    // holding `state`. Tests can put the executor back after a timed-out wait.
    Closing(Option<Executor>),
}

impl AsyncManager {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(ExecutorState::Stopped),
            state_changed: Condvar::new(),
            current_generation: AtomicU64::new(1),
            #[cfg(test)]
            after_generation_publish_hook: Mutex::new(None),
        }
    }

    pub(crate) fn start(&self, worker_count: usize) -> XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            ExecutorState::Stopped => {}
            ExecutorState::Running(_) => return Ok(()),
            ExecutorState::Closing(_) => return Err(XllError::Closing),
        }
        *state = ExecutorState::Running(Executor::start(worker_count, self.current_generation())?);
        Ok(())
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    pub(crate) fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> XllResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let result = {
            let state = self.state.lock();
            match &*state {
                ExecutorState::Running(_)
                    if generation != self.current_generation.load(Ordering::Acquire) =>
                {
                    Err(SpawnRejection {
                        error: cancelled_calculation_error(),
                        future,
                        cancellation,
                        cancel: true,
                    })
                }
                ExecutorState::Running(executor) => {
                    executor.spawn(generation, future, cancellation)
                }
                ExecutorState::Stopped | ExecutorState::Closing(_) => Err(SpawnRejection {
                    error: XllError::Closing,
                    future,
                    cancellation,
                    cancel: false,
                }),
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(rejection) => {
                if rejection.cancel {
                    cancel_source_no_unwind(&rejection.cancellation);
                }
                // Rejected futures and their captured user values must be
                // dropped after releasing the manager state mutex: Drop may
                // legitimately re-enter calculation/runtime APIs.
                drop(rejection.future);
                Err(rejection.error)
            }
        }
    }

    #[cfg(test)]
    fn cancel_generation(&self, generation: u64) {
        let tasks = match &*self.state.lock() {
            ExecutorState::Running(executor) => executor.cancel_generation(generation),
            ExecutorState::Stopped | ExecutorState::Closing(_) => Vec::new(),
        };
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn cancel_current_generation(&self) {
        let tasks = match &*self.state.lock() {
            ExecutorState::Running(executor) => {
                executor.cancel_generation(self.current_generation())
            }
            ExecutorState::Stopped | ExecutorState::Closing(_) => Vec::new(),
        };
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn advance_generation(&self) -> bool {
        let state = self.state.lock();
        let current = self.current_generation();
        let next = current.wrapping_add(1);
        let advanced = match &*state {
            ExecutorState::Stopped => {
                self.current_generation.store(next, Ordering::Release);
                true
            }
            ExecutorState::Running(executor) => {
                if !executor.advance_generation(next) {
                    false
                } else {
                    self.current_generation.store(next, Ordering::Release);
                    true
                }
            }
            ExecutorState::Closing(_) => false,
        };
        #[cfg(test)]
        if advanced {
            let hook = self.after_generation_publish_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        advanced
    }

    #[cfg(test)]
    pub(crate) fn set_after_generation_publish_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.after_generation_publish_hook.lock() = hook;
    }

    pub(crate) fn close(&self) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        let Some(executor) = self.take_executor_for_close() else {
            return crate::shutdown::StopOutcome {
                certificate: crate::shutdown::AsyncStopped::new(),
                issues: Vec::new(),
            };
        };
        let tasks = executor.request_close();
        // Manager state released — cancel/abort and run arbitrary task cleanup
        // without blocking re-entry into cancellation or generation APIs.
        cancel_tasks(tasks);

        // Excel owns the XLL module lifetime. Returning while a worker can
        // still execute this module is unsound, so shutdown deliberately has
        // no timeout: a non-cooperative poll keeps xlAutoClose blocked.
        if !executor.wait_for_idle() && !executor.drain_after_worker_failure() {
            // No worker remains that can release the outstanding task guards.
            // Returning an AsyncStopped certificate would permit unsafe unload.
            std::process::abort();
        }
        let issues = executor.finish_close();
        self.finish_close();
        crate::shutdown::StopOutcome {
            certificate: crate::shutdown::AsyncStopped::new(),
            issues,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(*self.state.lock(), ExecutorState::Stopped)
    }

    #[cfg(test)]
    fn close_with_timeout(&self, timeout: Duration) -> XllResult<()> {
        let Some(executor) = self.take_executor_for_close() else {
            return Ok(());
        };
        let tasks = executor.request_close();
        // Manager state released — cancel/abort without holding any locks.
        cancel_tasks(tasks);

        if !executor.wait_for_idle_timeout(timeout) {
            self.restore_closing_executor(executor);
            return Err(XllError::Internal {
                diagnostic_id: 0x4153_594e_5449_4d45,
            });
        }
        let issues = executor.finish_close();
        self.finish_close();
        if issues.is_empty() {
            Ok(())
        } else {
            Err(XllError::Panic)
        }
    }

    fn take_executor_for_close(&self) -> Option<Executor> {
        let mut state = self.state.lock();
        loop {
            match &*state {
                ExecutorState::Stopped => return None,
                ExecutorState::Running(_) | ExecutorState::Closing(Some(_)) => {
                    let previous = std::mem::replace(&mut *state, ExecutorState::Closing(None));
                    self.state_changed.notify_all();
                    return match previous {
                        ExecutorState::Running(executor)
                        | ExecutorState::Closing(Some(executor)) => Some(executor),
                        ExecutorState::Stopped | ExecutorState::Closing(None) => {
                            unreachable!("close ownership was checked while holding state")
                        }
                    };
                }
                ExecutorState::Closing(None) => self.state_changed.wait(&mut state),
            }
        }
    }

    #[cfg(test)]
    fn wait_for_closing(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while !matches!(*state, ExecutorState::Closing(_)) {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.state_changed.wait_for(&mut state, deadline - now);
        }
        true
    }

    #[cfg(test)]
    fn restore_closing_executor(&self, executor: Executor) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        *state = ExecutorState::Closing(Some(executor));
        self.state_changed.notify_all();
    }

    fn finish_close(&self) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        *state = ExecutorState::Stopped;
        self.state_changed.notify_all();
    }
}

struct Executor {
    inner: Arc<ExecutorInner>,
    sender: Sender<Runnable>,
    receiver: Receiver<Runnable>,
    workers: Vec<JoinHandle<()>>,
}

struct ExecutorInner {
    next_id: AtomicU64,
    active: AtomicUsize,
    live_workers: AtomicUsize,
    fatal_worker_failure: AtomicBool,
    registry: Mutex<TaskRegistry>,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

struct TaskRegistry {
    closing: bool,
    generations: HashMap<u64, CalculationGeneration>,
}

struct CalculationGeneration {
    id: u64,
    cancelled: bool,
    tasks: HashMap<u64, TaskControl>,
}

struct TaskControl {
    abort: AbortHandle,
    cancellation: CancellationSource,
}

struct SpawnRejection<F> {
    error: XllError,
    future: F,
    cancellation: CancellationSource,
    cancel: bool,
}

impl Executor {
    fn start(worker_count: usize, generation: u64) -> XllResult<Self> {
        let worker_count = worker_count.clamp(1, 32);
        let (sender, receiver) = async_channel::unbounded::<Runnable>();
        let inner = Arc::new(ExecutorInner {
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            fatal_worker_failure: AtomicBool::new(false),
            registry: Mutex::new(TaskRegistry {
                closing: false,
                generations: HashMap::from([(
                    generation,
                    CalculationGeneration {
                        id: generation,
                        cancelled: false,
                        tasks: HashMap::new(),
                    },
                )]),
            }),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        });
        let mut workers = scopeguard::guard(
            Vec::<JoinHandle<()>>::with_capacity(worker_count),
            |mut workers| {
                sender.close();
                loop {
                    let Some(worker) = workers.pop() else {
                        break;
                    };
                    drop(worker.join());
                }
            },
        );
        for index in 0..worker_count {
            let receiver = receiver.clone();
            let worker_inner = Arc::clone(&inner);
            inner.live_workers.fetch_add(1, Ordering::Release);
            let worker = thread::Builder::new()
                .name(format!("xlfn-async-{index}"))
                .spawn(move || {
                    let _exit = WorkerExitGuard {
                        inner: worker_inner,
                    };
                    run_executor(receiver);
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    inner.live_workers.fetch_sub(1, Ordering::AcqRel);
                    return Err(XllError::Internal {
                        diagnostic_id: 0x4153_594e_4353_504e,
                    });
                }
            };
            workers.push(worker);
        }
        let workers = scopeguard::ScopeGuard::into_inner(workers);
        Ok(Self {
            inner,
            sender,
            receiver,
            workers,
        })
    }

    fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> Result<(), SpawnRejection<F>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self
            .inner
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_PENDING).then_some(active + 1)
            })
            .is_err()
        {
            return Err(SpawnRejection {
                error: XllError::Overloaded,
                future,
                cancellation,
                cancel: false,
            });
        }
        let reservation = scopeguard::guard(Arc::clone(&self.inner), |inner| {
            release_active(&inner);
        });
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (abort, registration) = AbortHandle::new_pair();
        {
            let mut registry = self.inner.registry.lock();
            if registry.closing {
                drop(registry);
                return Err(SpawnRejection {
                    error: XllError::Closing,
                    future,
                    cancellation,
                    cancel: true,
                });
            }
            let current = registry
                .generations
                .get_mut(&generation)
                .expect("the current calculation generation is installed");
            debug_assert_eq!(current.id, generation);
            if current.cancelled {
                drop(registry);
                return Err(SpawnRejection {
                    error: cancelled_calculation_error(),
                    future,
                    cancellation,
                    cancel: true,
                });
            }
            current.tasks.insert(
                id,
                TaskControl {
                    abort,
                    cancellation,
                },
            );
        }
        let completion = CompletionGuard {
            inner: Arc::clone(&self.inner),
            generation,
            id,
        };
        drop(scopeguard::ScopeGuard::into_inner(reservation));
        let wrapped = async move {
            let _completion = completion;
            let _ = Abortable::new(future, registration).await;
        };
        let sender = self.sender.clone();
        let schedule = move |runnable| {
            let _ = sender.try_send(runnable);
        };
        let (runnable, task) = async_task::spawn(wrapped, schedule);
        task.detach();
        runnable.schedule();
        Ok(())
    }

    fn cancel_generation(&self, generation: u64) -> Vec<TaskControl> {
        let mut registry = self.inner.registry.lock();
        let Some(state) = registry.generations.get_mut(&generation) else {
            return Vec::new();
        };
        debug_assert_eq!(state.id, generation);
        state.cancelled = true;
        state.tasks.drain().map(|(_, task)| task).collect()
    }

    fn advance_generation(&self, next: u64) -> bool {
        let mut registry = self.inner.registry.lock();
        if registry.closing {
            return false;
        }
        registry
            .generations
            .entry(next)
            .or_insert_with(|| CalculationGeneration {
                id: next,
                cancelled: false,
                tasks: HashMap::new(),
            });
        registry
            .generations
            .retain(|generation, state| *generation == next || !state.tasks.is_empty());
        true
    }

    fn request_close(&self) -> Vec<TaskControl> {
        let mut registry = self.inner.registry.lock();
        registry.closing = true;
        registry
            .generations
            .values_mut()
            .flat_map(|generation| generation.tasks.drain().map(|(_, task)| task))
            .collect()
    }

    fn wait_for_idle(&self) -> bool {
        let mut guard = self.inner.wait_lock.lock();
        while self.inner.active.load(Ordering::Acquire) != 0 {
            if self.inner.fatal_worker_failure.load(Ordering::Acquire)
                && self.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            self.inner.idle.wait(&mut guard);
        }
        true
    }

    #[cfg(test)]
    fn wait_for_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.wait_lock.lock();
        while self.inner.active.load(Ordering::Acquire) != 0 {
            if self.inner.fatal_worker_failure.load(Ordering::Acquire)
                && self.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.inner.idle.wait_for(&mut guard, deadline - now);
        }
        true
    }

    fn drain_after_worker_failure(&self) -> bool {
        self.sender.close();
        while let Ok(runnable) = self.receiver.try_recv() {
            drop(runnable);
        }
        self.inner.active.load(Ordering::Acquire) == 0
    }

    fn finish_close(mut self) -> Vec<crate::shutdown::CleanupIssue> {
        self.sender.close();
        let mut issues = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                issues.push(crate::shutdown::CleanupIssue {
                    component: "async worker",
                    kind: crate::CleanupIssueKind::WorkerPanickedAfterJoin,
                    error: XllError::Panic,
                });
            }
        }
        issues
    }
}

struct WorkerExitGuard {
    inner: Arc<ExecutorInner>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.inner
                .fatal_worker_failure
                .store(true, Ordering::Release);
        }
        self.inner.live_workers.fetch_sub(1, Ordering::AcqRel);
        let _guard = self.inner.wait_lock.lock();
        self.inner.idle.notify_all();
    }
}

fn release_active(inner: &ExecutorInner) {
    if inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
        let _guard = inner.wait_lock.lock();
        inner.idle.notify_all();
    }
}

struct CompletionGuard {
    inner: Arc<ExecutorInner>,
    generation: u64,
    id: u64,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let mut registry = self.inner.registry.lock();
        if let Some(generation) = registry.generations.get_mut(&self.generation) {
            generation.tasks.remove(&self.id);
        }
        drop(registry);
        release_active(&self.inner);
    }
}

fn cancelled_calculation_error() -> XllError {
    XllError::ExcelValue(crate::ExcelError::NotAvailable)
}

/// Cancels and aborts a batch of tasks outside of any lock.
fn cancel_tasks(tasks: Vec<TaskControl>) {
    for TaskControl {
        abort,
        cancellation,
    } in tasks
    {
        // CancellationToken is public and may have been polled by an arbitrary
        // executor. Its Waker::wake implementation is user-controlled and may
        // panic; one such panic must not prevent this or later tasks from being
        // aborted, especially while AsyncManager owns a Closing executor.
        cancel_source_no_unwind(&cancellation);
        let _ = catch_unwind(AssertUnwindSafe(|| abort.abort()));
    }
}

fn cancel_source_no_unwind(cancellation: &CancellationSource) {
    let _ = catch_unwind(AssertUnwindSafe(|| cancellation.cancel()));
}

fn run_executor(receiver: Receiver<Runnable>) {
    loop {
        let Ok(runnable) = receiver.recv_blocking() else {
            break;
        };
        // Panics from user futures are contained by the UDF wrapper. Anything
        // that reaches this executor boundary is an infrastructure failure and
        // must terminate the worker so close can report it and explicitly
        // dispose any work stranded after the last worker exits.
        runnable.run();
    }
}

struct OwnedAsyncHandle {
    udf_id: &'static str,
    raw: XLOPER12,
    bytes: Option<Box<[u8]>>,
    completed: bool,
    fallback_error: Option<XllError>,
}

// SAFETY: construction owns any pointed-to bytes; an opaque zero-length handle
// is only copied back to Excel and is never dereferenced by Rust.
unsafe impl Send for OwnedAsyncHandle {}

impl OwnedAsyncHandle {
    unsafe fn from_raw(udf_id: &'static str, raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: the caller guarantees a live Excel async-handle argument.
        let value = unsafe { raw.as_ref() }.ok_or_else(|| {
            XllError::input(
                "async_handle",
                crate::InputError::Malformed("null async handle"),
            )
        })?;
        if value.base_type() != XLTYPE_BIG_DATA {
            return Err(XllError::input(
                "async_handle",
                crate::InputError::Malformed("expected xltypeBigData"),
            ));
        }
        // SAFETY: XLTYPE_BIG_DATA selects the big_data union field.
        let big_data = unsafe { value.value.big_data };
        let byte_count = usize::try_from(big_data.byte_count).map_err(|_| {
            XllError::input(
                "async_handle",
                crate::InputError::Malformed("negative async handle size"),
            )
        })?;
        if byte_count > MAX_ASYNC_HANDLE_BYTES {
            return Err(XllError::input(
                "async_handle",
                crate::InputError::Malformed("async handle is too large"),
            ));
        }
        let mut bytes = if byte_count == 0 {
            None
        } else {
            // SAFETY: a positive byte count selects the data pointer representation.
            let data = unsafe { big_data.handle.data };
            if data.is_null() {
                return Err(XllError::input(
                    "async_handle",
                    crate::InputError::Malformed("null async handle data"),
                ));
            }
            // SAFETY: Excel promises byte_count readable bytes for this call.
            Some(
                unsafe { std::slice::from_raw_parts(data, byte_count) }
                    .to_vec()
                    .into_boxed_slice(),
            )
        };
        let handle = bytes
            .as_mut()
            .map_or(big_data.handle, |bytes| XLOPER12BigDataHandle {
                data: bytes.as_mut_ptr(),
            });
        Ok(Self {
            udf_id,
            raw: XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle,
                        byte_count: big_data.byte_count,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            },
            bytes,
            completed: false,
            fallback_error: None,
        })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _ = &self.bytes;
        NonNull::from(&mut self.raw)
    }

    fn set_error(&mut self, error: XllError) {
        self.fallback_error = Some(error);
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for OwnedAsyncHandle {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            let error = self
                .fallback_error
                .take()
                .unwrap_or(XllError::ExcelValue(crate::ExcelError::NotAvailable));
            // SAFETY: raw is live and owned by this handle.
            unsafe {
                return_error(self.udf_id, &mut self.raw, &error);
            }
        }
    }
}

struct AsyncCompletionTracker {
    udf_id: &'static str,
    excel_name: &'static str,
    call_id: CallId,
    calculation_id: crate::execution::CalculationId,
    started_at: SystemTime,
    concurrent_calls: usize,
    started: Instant,
    layers: Option<crate::execution::EnteredLayers>,
    completed: bool,
}

impl AsyncCompletionTracker {
    fn new(
        metadata: &CallMetadata,
        started: Instant,
        layers: crate::execution::EnteredLayers,
    ) -> Self {
        Self {
            udf_id: metadata.udf_id,
            excel_name: metadata.excel_name,
            call_id: metadata.call_id,
            calculation_id: metadata.calculation_id,
            started_at: metadata.started_at,
            concurrent_calls: metadata.concurrent_calls,
            started,
            layers: Some(layers),
            completed: false,
        }
    }

    fn finish(&mut self, outcome: &CallOutcome<'_>) {
        if !self.completed {
            self.completed = true;
            if let Some(layers) = self.layers.take() {
                layers.exit(outcome);
            }
            let metadata = CallMetadata {
                udf_id: self.udf_id,
                excel_name: self.excel_name,
                call_id: self.call_id,
                calculation_id: self.calculation_id,
                started_at: self.started_at,
                concurrent_calls: self.concurrent_calls,
            };
            crate::execution::trace(&metadata, outcome);
        }
    }

    fn finish_error(&mut self, error: &XllError) {
        if !self.completed {
            crate::diagnostics::report_no_unwind(self.udf_id, error);
            let outcome = crate::execution::outcome_for_error(error, self.started.elapsed());
            self.finish(&outcome);
        }
    }
}

impl Drop for AsyncCompletionTracker {
    fn drop(&mut self) {
        if !self.completed {
            let error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
            self.finish_error(&error);
        }
    }
}

/// Runs the synchronous launch portion of a native Excel async UDF.
///
/// # Safety
///
/// `raw_handle` must point to a valid, aligned, Excel-owned `XLOPER12` async
/// handle that remains live for the duration of this call.
#[doc(hidden)]
pub unsafe fn async_udf_boundary_named<S, Start, Fut, T>(
    runtime: &'static Runtime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    S: Send + Sync + 'static,
    Start: FnOnce(Arc<S>, CancellationToken) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: IntoExcelValue + Send + 'static,
{
    let call_id = runtime.next_call_id();
    let started = Instant::now();
    let started_at = SystemTime::now();
    let guard = match runtime.enter() {
        Ok(guard) => guard,
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let concurrent_calls = guard.concurrent_calls();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id: CallId::from(call_id),
        calculation_id: runtime.calculation_id(),
        started_at,
        concurrent_calls,
    };
    let layers = match crate::execution::EnteredLayers::enter(&runtime.layers(), &metadata) {
        Ok(layers) => layers,
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let tracker = Arc::new(Mutex::new(AsyncCompletionTracker::new(
        &metadata, started, layers,
    )));

    // Excel does not raise CalculationEnded/CalculationCanceled for every
    // programmatic recalculation, so the public token cannot promise complete
    // calculation scoping even though event-driven generations are linearized.
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut handle = match unsafe { OwnedAsyncHandle::from_raw(udf_id, raw_handle) } {
        Ok(handle) => handle,
        Err(error) => {
            tracker.lock().finish_error(&error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let future = catch_unwind(AssertUnwindSafe(|| start(guard.state_arc(), token.clone())))
        .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let tracker_task = Arc::clone(&tracker);
            let task = async move {
                let evaluated = AssertUnwindSafe(future).catch_unwind().await;
                #[cfg(test)]
                if let Some(hook) = *AFTER_ASYNC_EVALUATION_HOOK.lock() {
                    hook();
                }

                // Linearize delivery vs cancellation using CAS on the delivery state machine.
                if !token.try_start_delivery() {
                    let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
                    handle.set_error(cancel_error.clone());
                    tracker_task.lock().finish_error(&cancel_error);
                    return;
                }

                let result = match evaluated {
                    Ok(Ok(value)) => catch_unwind(AssertUnwindSafe(|| {
                        let value = value.into_excel_value()?;
                        AsyncReturnPointer::from_value(value)
                    }))
                    .unwrap_or(Err(XllError::Panic)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(XllError::Panic),
                };
                let (pointer, computation_error) = match result {
                    Ok(pointer) => (pointer, None),
                    Err(error) => (AsyncReturnPointer::error(&error), Some(error)),
                };
                // SAFETY: both pointers are owned and live for the callback.
                let delivery = unsafe {
                    let delivery = async_return(handle.pointer(), pointer.as_non_null());
                    handle.complete();
                    delivery
                };
                token.finish_delivery();
                match delivery {
                    Ok(()) => match computation_error {
                        Some(error) => tracker_task.lock().finish_error(&error),
                        None => {
                            let outcome = CallOutcome {
                                result: UdfResultKind::Success,
                                error: None,
                                vendor_code: None,
                                duration: started.elapsed(),
                            };
                            tracker_task.lock().finish(&outcome);
                        }
                    },
                    Err(error) => tracker_task.lock().finish_error(&error),
                }
            };
            if let Err(error) =
                runtime
                    .async_manager()
                    .spawn(metadata.calculation_id.get(), task, cancellation)
            {
                tracker.lock().finish_error(&error);
            }
        }
        Err(error) => {
            handle.set_error(error.clone());
            tracker.lock().finish_error(&error);
        }
    }
}

unsafe fn return_error(udf_id: &'static str, handle: *mut XLOPER12, error: &XllError) {
    let Some(handle) = NonNull::new(handle) else {
        crate::diagnostics::report_no_unwind(
            udf_id,
            &XllError::input(
                "async_handle",
                crate::InputError::Malformed("null async handle"),
            ),
        );
        return;
    };
    let pointer = AsyncReturnPointer::error(error);
    // SAFETY: the RAII-owned return is live for the callback.
    unsafe {
        if let Err(delivery_error) = async_return(handle, pointer.as_non_null()) {
            crate::diagnostics::report_no_unwind(udf_id, &delivery_error);
        }
    }
}

unsafe fn async_return(handle: NonNull<XLOPER12>, result: NonNull<XLOPER12>) -> XllResult<()> {
    #[cfg(test)]
    if let Some(hook) = *ASYNC_RETURN_HOOK.lock() {
        return hook(handle.as_ptr(), result.as_ptr());
    }
    let arguments = [handle, result];
    // SAFETY: both XLOPER12 pointers are live for this call.
    let (status, mut callback_value) =
        unsafe { crate::callback_value::ExcelCallbackValue::call(XL_ASYNC_RETURN, &arguments) };
    let accepted = status == XLRET_SUCCESS
        && callback_value.base_type()? == XLTYPE_BOOL
        // SAFETY: XLTYPE_BOOL selects the boolean union field.
        && unsafe { callback_value.raw()?.value.boolean != 0 };
    callback_value.try_release()?;
    if !accepted {
        let error = XllError::ExcelApi {
            function: "xlAsyncReturn",
            code: if status == XLRET_SUCCESS { -1 } else { status },
        };
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
type AsyncReturnHook = fn(*mut XLOPER12, *mut XLOPER12) -> XllResult<()>;
#[cfg(test)]
static ASYNC_RETURN_HOOK: Mutex<Option<AsyncReturnHook>> = Mutex::new(None);
#[cfg(test)]
static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub fn cancel_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.cancel_async();
}

pub fn end_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.finish_calculation();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const TEST_GENERATION: u64 = 1;
    static CALLBACK_SENDER: Mutex<Option<std::sync::mpsc::Sender<i32>>> = Mutex::new(None);
    static EVALUATION_BARRIER: Mutex<
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    > = Mutex::new(None);

    fn stop_after_async_evaluation() {
        let barrier = EVALUATION_BARRIER.lock();
        let (reached, release) = barrier.as_ref().expect("evaluation barrier is installed");
        reached.send(()).unwrap();
        release.recv().unwrap();
    }

    fn test_cancellation_source() -> CancellationSource {
        CancellationSource::new(CancellationGuarantee::BestEffort).0
    }

    fn record_callback(_handle: *mut XLOPER12, result: *mut XLOPER12) -> XllResult<()> {
        // SAFETY: async_return invokes the hook synchronously with a live result.
        let result = unsafe { &*result };
        let value = if result.base_type() == xlfn_sys::XLTYPE_NUM {
            // SAFETY: XLTYPE_NUM selects the number field.
            unsafe { result.value.number as i32 }
        } else {
            -1
        };
        if let Some(sender) = CALLBACK_SENDER.lock().as_ref() {
            sender.send(value).unwrap();
        }
        Ok(())
    }

    fn reject_callback(_handle: *mut XLOPER12, _result: *mut XLOPER12) -> XllResult<()> {
        Err(XllError::ExcelApi {
            function: "xlAsyncReturn",
            code: xlfn_sys::XLRET_FAILED,
        })
    }

    #[test]
    fn executor_runs_tasks_and_joins_on_close() {
        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = Arc::clone(&completed);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    task_completed.store(true, Ordering::Release);
                    done_tx.send(()).unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(manager.close().issues.is_empty());
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_drops_pending_future_without_running_its_tail() {
        struct DropSignal(Arc<AtomicBool>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(Arc::clone(&dropped));
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _signal = signal;
                    std::future::pending::<()>().await;
                },
                test_cancellation_source(),
            )
            .unwrap();
        manager.cancel_generation(TEST_GENERATION);
        assert!(manager.close().issues.is_empty());
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn rejected_spawn_drops_future_after_releasing_manager_state() {
        struct ReentrantRejectedFuture {
            manager: Arc<AsyncManager>,
            dropped: std::sync::mpsc::Sender<()>,
        }

        impl Future for ReentrantRejectedFuture {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        impl Drop for ReentrantRejectedFuture {
            fn drop(&mut self) {
                self.manager.cancel_generation(TEST_GENERATION);
                self.dropped.send(()).unwrap();
            }
        }

        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        manager.cancel_generation(TEST_GENERATION);
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let future = ReentrantRejectedFuture {
            manager: Arc::clone(&manager),
            dropped: dropped_tx,
        };
        let spawning_manager = Arc::clone(&manager);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let spawning = std::thread::spawn(move || {
            result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, future, test_cancellation_source()))
                .unwrap();
        });

        assert!(matches!(
            result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        spawning.join().unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn close_isolates_panicking_cancellation_waker_and_completes_shutdown() {
        struct PanicWake;

        impl std::task::Wake for PanicWake {
            fn wake(self: Arc<Self>) {
                panic!("injected async close waker panic");
            }

            fn wake_by_ref(self: &Arc<Self>) {
                panic!("injected async close waker panic");
            }
        }

        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let panic_waker = std::task::Waker::from(Arc::new(PanicWake));
        let mut waiter = Box::pin(token.cancelled());
        assert_eq!(
            waiter
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&panic_waker)),
            std::task::Poll::Pending
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let drop_signal = DropSignal(Arc::clone(&dropped));
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _drop_signal = drop_signal;
                    std::future::pending::<()>().await;
                },
                source,
            )
            .unwrap();

        assert!(manager.close().issues.is_empty());
        assert!(token.is_cancelled());
        assert!(dropped.load(Ordering::Acquire));

        // A completed close must leave no orphaned Closing(None) owner.
        assert!(manager.advance_generation());
        manager.start(1).unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn close_allows_aborted_future_drop_to_reenter_runtime() {
        struct ReentrantDrop {
            runtime: &'static Runtime<()>,
            dropped: std::sync::mpsc::Sender<()>,
        }

        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                cancel_async_calculation(self.runtime);
                end_async_calculation(self.runtime);
                self.dropped.send(()).unwrap();
            }
        }

        let runtime: &'static Runtime<()> = Box::leak(Box::new(Runtime::new()));
        runtime.start_async(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let reentrant = ReentrantDrop {
            runtime,
            dropped: dropped_tx,
        };
        runtime
            .async_manager()
            .spawn(
                runtime.calculation_id().get(),
                async move {
                    let _reentrant = reentrant;
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::future::pending::<()>().await;
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(
                    runtime
                        .async_manager()
                        .close_with_timeout(Duration::from_secs(2)),
                )
                .unwrap();
        });
        assert!(
            runtime
                .async_manager()
                .wait_for_closing(Duration::from_secs(1))
        );
        assert!(matches!(runtime.start_async(1), Err(XllError::Closing)));
        release_tx.send(()).unwrap();

        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn close_allows_aborted_layer_cleanup_to_reenter_runtime() {
        struct ReentrantLayer {
            runtime: &'static Runtime<u32>,
            exited: std::sync::mpsc::Sender<()>,
        }
        struct ReentrantLayerGuard {
            runtime: &'static Runtime<u32>,
            exited: std::sync::mpsc::Sender<()>,
        }

        impl crate::UdfLayer for ReentrantLayer {
            fn enter(&self, _: &crate::CallMetadata) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(ReentrantLayerGuard {
                    runtime: self.runtime,
                    exited: self.exited.clone(),
                }))
            }
        }

        impl crate::UdfLayerGuard for ReentrantLayerGuard {
            fn exit(self: Box<Self>, _: &crate::CallOutcome<'_>) {
                cancel_async_calculation(self.runtime);
                end_async_calculation(self.runtime);
                self.exited.send(()).unwrap();
            }
        }

        let _guard = TEST_LOCK.lock().unwrap();
        let runtime: &'static Runtime<u32> = Box::leak(Box::new(Runtime::new()));
        let (exited_tx, exited_rx) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            7_u32,
            vec![Arc::new(ReentrantLayer {
                runtime,
                exited: exited_tx,
            })],
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        *ASYNC_RETURN_HOOK.lock() = Some(record_callback);
        *CALLBACK_SENDER.lock() = None;

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_reentrant_layer_close",
                "TEST.ASYNC.REENTRANT.LAYER.CLOSE",
                &mut handle,
                move |_, _| {
                    Ok(async move {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        std::future::pending::<()>().await;
                        Ok::<_, XllError>(42.0)
                    })
                },
            );
        }
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(
                    runtime
                        .async_manager()
                        .close_with_timeout(Duration::from_secs(2)),
                )
                .unwrap();
        });
        assert!(
            runtime
                .async_manager()
                .wait_for_closing(Duration::from_secs(1))
        );
        release_tx.send(()).unwrap();

        exited_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
        *ASYNC_RETURN_HOOK.lock() = None;
    }

    #[test]
    fn cancellation_token_is_signaled_before_task_abort() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let observed = token.clone();
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _token = token;
                    std::future::pending::<()>().await;
                },
                source,
            )
            .unwrap();
        manager.cancel_generation(TEST_GENERATION);
        assert!(observed.is_cancelled());
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn cancelled_generation_rejects_late_spawn_and_next_generation_accepts_work() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        manager.cancel_generation(TEST_GENERATION);

        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let next = TEST_GENERATION + 1;
        assert!(manager.advance_generation());
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        manager
            .spawn(next, async {}, test_cancellation_source())
            .unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn cancelling_new_generation_does_not_cancel_live_work_from_previous_generation() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (old_source, old_token) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        manager
            .spawn(TEST_GENERATION, std::future::pending(), old_source)
            .unwrap();

        let next = TEST_GENERATION + 1;
        assert!(manager.advance_generation());
        let (new_source, new_token) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        manager
            .spawn(next, std::future::pending(), new_source)
            .unwrap();

        manager.cancel_generation(next);
        assert!(new_token.is_cancelled());
        assert!(!old_token.is_cancelled());

        assert!(manager.close().issues.is_empty());
        assert!(old_token.is_cancelled());
    }

    #[test]
    fn spawn_and_cancel_are_linearized_by_the_generation_registry() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);

        let spawning_manager = Arc::clone(&manager);
        let spawning_barrier = Arc::clone(&barrier);
        let spawning = std::thread::spawn(move || {
            spawning_barrier.wait();
            spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source)
        });

        let cancelling_manager = Arc::clone(&manager);
        let cancelling_barrier = Arc::clone(&barrier);
        let cancelling = std::thread::spawn(move || {
            cancelling_barrier.wait();
            cancelling_manager.cancel_generation(TEST_GENERATION);
        });

        barrier.wait();
        let spawn_result = spawning.join().unwrap();
        cancelling.join().unwrap();

        match spawn_result {
            Ok(()) => assert!(token.is_cancelled()),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable)) => {
                assert!(token.is_cancelled());
            }
            Err(error) => panic!("unexpected spawn result: {error}"),
        }
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn joined_worker_panic_is_a_cleanup_issue_with_a_stop_certificate() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("injected task panic");
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        let outcome = manager.close();
        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(
            outcome.issues[0].kind,
            crate::CleanupIssueKind::WorkerPanickedAfterJoin
        );
        let _stopped = outcome.certificate;
        assert!(manager.is_stopped());
    }

    #[test]
    fn lone_worker_panic_drops_tasks_left_on_the_queue() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("injected worker-fatal panic");
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        manager
            .spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            )
            .unwrap();

        let closing = Arc::clone(&manager);
        let closer = std::thread::spawn(move || closing.close());
        release_tx.send(()).unwrap();
        let outcome = closer.join().unwrap();

        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(
            outcome.issues[0].kind,
            crate::CleanupIssueKind::WorkerPanickedAfterJoin
        );
        assert!(manager.is_stopped());
    }

    #[test]
    fn pending_task_limit_is_reserved_atomically() {
        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        for _ in 0..MAX_PENDING {
            manager
                .spawn(
                    TEST_GENERATION,
                    std::future::pending(),
                    test_cancellation_source(),
                )
                .unwrap();
        }
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::Overloaded)
        ));
        manager.cancel_generation(TEST_GENERATION);
        manager.close_with_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn shutdown_timeout_refuses_close_until_blocking_poll_returns() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            manager
                .close_with_timeout(Duration::from_millis(10))
                .is_err()
        );
        release_tx.send(()).unwrap();
        manager.close_with_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn production_close_waits_until_blocking_poll_returns() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let closer_manager = Arc::clone(&manager);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            assert!(closer_manager.close().issues.is_empty());
            closed_tx.send(()).unwrap();
        });
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn async_handle_payload_is_deep_copied() {
        let mut bytes = vec![1_u8, 2, 3, 4];
        let original = bytes.as_mut_ptr();
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle { data: original },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: raw is a live, well-formed test async handle.
        let mut owned = unsafe { OwnedAsyncHandle::from_raw("test_payload", &mut raw) }.unwrap();
        // SAFETY: the owned value remains XLTYPE_BIG_DATA with a positive size.
        let copied = unsafe { owned.raw.value.big_data.handle.data };
        assert_ne!(copied, original);
        bytes.fill(9);
        assert_eq!(
            // SAFETY: copied points to the owned four-byte payload.
            unsafe { std::slice::from_raw_parts(copied, 4) },
            &[1, 2, 3, 4]
        );
        owned.complete();
    }

    #[test]
    fn async_boundary_returns_completed_value_through_callback() {
        let _guard = TEST_LOCK.lock().unwrap();
        let runtime = Box::leak(Box::new(Runtime::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let (sender, receiver) = std::sync::mpsc::channel();
        *CALLBACK_SENDER.lock() = Some(sender);
        *ASYNC_RETURN_HOOK.lock() = Some(record_callback);
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async",
                "TEST.ASYNC",
                &mut handle,
                |_, token| {
                    assert_eq!(token.guarantee(), CancellationGuarantee::BestEffort);
                    Ok(async { Ok::<_, XllError>(42.0) })
                },
            );
        }
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), 42);
        assert!(runtime.close_async().issues.is_empty());
        *ASYNC_RETURN_HOOK.lock() = None;
        *CALLBACK_SENDER.lock() = None;
    }

    #[test]
    fn async_boundary_reports_handler_failures_to_layers() {
        struct Recorder(std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>);
        struct RecorderGuard {
            sender: std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>,
            concurrent_calls: usize,
        }
        impl crate::UdfLayer for Recorder {
            fn enter(
                &self,
                metadata: &crate::CallMetadata,
            ) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(RecorderGuard {
                    sender: self.0.clone(),
                    concurrent_calls: metadata.concurrent_calls,
                }))
            }
        }
        impl crate::UdfLayerGuard for RecorderGuard {
            fn exit(self: Box<Self>, outcome: &crate::CallOutcome<'_>) {
                self.sender
                    .send((outcome.result, outcome.vendor_code, self.concurrent_calls))
                    .unwrap();
            }
        }

        let _guard = TEST_LOCK.lock().unwrap();
        let runtime = Box::leak(Box::new(Runtime::new()));
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, vec![Arc::new(Recorder(event_sender))]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let (sender, receiver) = std::sync::mpsc::channel();
        *CALLBACK_SENDER.lock() = Some(sender);
        *ASYNC_RETURN_HOOK.lock() = Some(record_callback);
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_failure",
                "TEST.ASYNC.FAILURE",
                &mut handle,
                |_, _| {
                    Ok(async {
                        Err::<f64, _>(XllError::Native {
                            code: 73,
                            message: "injected async failure".to_owned(),
                        })
                    })
                },
            );
        }
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), -1);
        let event = event_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.0, UdfResultKind::VendorError);
        assert_eq!(event.1, Some(73));
        assert_eq!(event.2, 1);

        assert!(runtime.close_async().issues.is_empty());
        *ASYNC_RETURN_HOOK.lock() = None;
        *CALLBACK_SENDER.lock() = None;
    }

    #[test]
    fn async_boundary_records_delivery_rejection_as_failure() {
        struct Recorder(std::sync::mpsc::Sender<UdfResultKind>);
        struct RecorderGuard(std::sync::mpsc::Sender<UdfResultKind>);

        impl crate::UdfLayer for Recorder {
            fn enter(&self, _: &crate::CallMetadata) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(RecorderGuard(self.0.clone())))
            }
        }

        impl crate::UdfLayerGuard for RecorderGuard {
            fn exit(self: Box<Self>, outcome: &crate::CallOutcome<'_>) {
                self.0.send(outcome.result).unwrap();
            }
        }

        let _guard = TEST_LOCK.lock().unwrap();
        let runtime = Box::leak(Box::new(Runtime::new()));
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, vec![Arc::new(Recorder(event_sender))]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        *ASYNC_RETURN_HOOK.lock() = Some(reject_callback);

        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_delivery_failure",
                "TEST.ASYNC.DELIVERY.FAILURE",
                &mut handle,
                |_, _| Ok(async { Ok::<_, XllError>(42.0) }),
            );
        }

        assert_eq!(
            event_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            UdfResultKind::InternalError
        );
        assert!(runtime.close_async().issues.is_empty());
        *ASYNC_RETURN_HOOK.lock() = None;
    }

    #[test]
    fn async_boundary_returns_error_on_cancellation() {
        let _guard = TEST_LOCK.lock().unwrap();
        let runtime = Box::leak(Box::new(Runtime::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let (sender, receiver) = std::sync::mpsc::channel();
        *CALLBACK_SENDER.lock() = Some(sender);
        *ASYNC_RETURN_HOOK.lock() = Some(record_callback);
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_cancel",
                "TEST.ASYNC.CANCEL",
                &mut handle,
                move |_, _| {
                    let release_rx = release_rx;
                    Ok(async move {
                        let _ = release_rx.recv();
                        Ok::<_, XllError>(123.0)
                    })
                },
            );
        }
        // Cancel all running async tasks. OwnedAsyncHandle::drop should fire and return error to hook.
        cancel_async_calculation(runtime);
        drop(release_tx);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), -1);

        assert!(runtime.close_async().issues.is_empty());
        *ASYNC_RETURN_HOOK.lock() = None;
        *CALLBACK_SENDER.lock() = None;
    }

    #[test]
    fn cancellation_after_evaluation_does_not_leak_the_return_block() {
        let _guard = TEST_LOCK.lock().unwrap();
        let before = crate::return_value::live_return_blocks();
        let runtime = Box::leak(Box::new(Runtime::new()));
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();

        let (callback_tx, callback_rx) = std::sync::mpsc::channel();
        *CALLBACK_SENDER.lock() = Some(callback_tx);
        *ASYNC_RETURN_HOOK.lock() = Some(record_callback);
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *EVALUATION_BARRIER.lock() = Some((reached_tx, release_rx));
        *AFTER_ASYNC_EVALUATION_HOOK.lock() = Some(stop_after_async_evaluation);

        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_cancel_after_evaluation",
                "TEST.ASYNC.CANCEL.AFTER.EVALUATION",
                &mut handle,
                |_, _| Ok(async { Ok::<_, XllError>("allocated return payload".to_owned()) }),
            );
        }

        reached_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel_async_calculation(runtime);
        release_tx.send(()).unwrap();
        assert_eq!(
            callback_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            -1
        );
        assert!(runtime.close_async().issues.is_empty());

        assert_eq!(crate::return_value::live_return_blocks(), before);
        *AFTER_ASYNC_EVALUATION_HOOK.lock() = None;
        *EVALUATION_BARRIER.lock() = None;
        *ASYNC_RETURN_HOOK.lock() = None;
        *CALLBACK_SENDER.lock() = None;
    }
}
