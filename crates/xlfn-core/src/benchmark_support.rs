//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]
#![allow(unsafe_code, reason = "Benchmark-only XLOPER12 pointer construction")]

#[cfg(feature = "async")]
use crate::CancellationGuarantee;
use crate::ExcelParameter;
#[cfg(feature = "async")]
use crate::XllError;
#[cfg(feature = "async")]
use crate::async_udf::AsyncManager;
#[cfg(feature = "async")]
use crate::cancellation::CancellationSource;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

#[cfg(feature = "async")]
pub struct AsyncSpawnBenchmark {
    manager: Arc<AsyncManager>,
    start_tx: Vec<std::sync::mpsc::SyncSender<usize>>,
    done_rx: std::sync::mpsc::Receiver<SpawnBatchResult>,
    producers: Vec<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "async")]
#[derive(Default)]
pub struct SpawnBatchResult {
    pub accepted: usize,
    pub overloaded: usize,
    pub other_errors: usize,
}

#[cfg(feature = "async")]
impl AsyncSpawnBenchmark {
    pub fn new(worker_count: usize, producer_count: usize) -> Self {
        assert!(producer_count != 0);

        let manager = Arc::new(AsyncManager::new());
        manager
            .start(worker_count)
            .expect("AsyncManager failed to start for benchmark");
        let generation = manager.current_generation();

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(producer_count);
        let mut start_tx = Vec::with_capacity(producer_count);
        let mut producers = Vec::with_capacity(producer_count);

        for _ in 0..producer_count {
            let (producer_tx, producer_rx) = std::sync::mpsc::sync_channel::<usize>(1);
            let manager = Arc::clone(&manager);
            let done_tx = done_tx.clone();

            start_tx.push(producer_tx);
            producers.push(std::thread::spawn(move || {
                while let Ok(iterations_per_thread) = producer_rx.recv() {
                    let mut result = SpawnBatchResult::default();

                    for _ in 0..iterations_per_thread {
                        let (source, _token) =
                            CancellationSource::new(CancellationGuarantee::BestEffort);

                        match manager.spawn(generation, async {}, source) {
                            Ok(()) => result.accepted += 1,
                            Err(XllError::Overloaded) => result.overloaded += 1,
                            Err(_) => result.other_errors += 1,
                        }
                    }

                    done_tx
                        .send(result)
                        .expect("benchmark driver receives producer result");
                }
            }));
        }

        Self {
            manager,
            start_tx,
            done_rx,
            producers,
        }
    }

    pub fn run(&self, iterations_per_thread: usize) -> SpawnBatchResult {
        for start in &self.start_tx {
            start
                .send(iterations_per_thread)
                .expect("benchmark producer receives start signal");
        }

        let mut total = SpawnBatchResult::default();
        for _ in 0..self.start_tx.len() {
            let result = self
                .done_rx
                .recv()
                .expect("benchmark producer finished batch");
            total.accepted += result.accepted;
            total.overloaded += result.overloaded;
            total.other_errors += result.other_errors;
        }

        total
    }
}

#[cfg(feature = "async")]
impl Drop for AsyncSpawnBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for producer in self.producers.drain(..) {
            producer.join().expect("benchmark producer panicked");
        }
        let _ = self.manager.close();
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SyncBenchKind {
    IngressUdfOnly,
    FullAdmission,
    ScalarReturnNoSubscriber,
    ScalarReturnUdfTraceEnabled,
    ReturnTrackerOnly,
}

#[derive(Clone, Copy)]
struct BenchmarkSubscriber;

impl tracing::Subscriber for BenchmarkSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == crate::execution::UDF_TRACE_TARGET
            && *metadata.level() <= tracing::Level::INFO
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn install_benchmark_subscriber() -> tracing::dispatcher::DefaultGuard {
    let dispatch = tracing::Dispatch::new(BenchmarkSubscriber);
    tracing::dispatcher::set_default(&dispatch)
}

pub struct SyncBoundaryWorkerPool {
    _runtime: &'static crate::Runtime<()>,
    threads: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl SyncBoundaryWorkerPool {
    pub fn new(threads: usize, iterations_per_thread: usize, kind: SyncBenchKind) -> Self {
        let ingress = crate::ingress::global_ingress();
        if ingress.phase() != crate::ingress::PHASE_CLOSED {
            ingress.begin_close_with(|| {});
            let _ = ingress.seal_and_drain();
        }
        let runtime: &'static crate::Runtime<()> = Box::leak(Box::new(crate::Runtime::<()>::new()));
        let close_epoch = runtime.close_epoch();
        let mut open_attempt = runtime
            .begin_open_if_epoch(close_epoch)
            .expect("begin_open");
        runtime.publish((), ());
        runtime
            .finish_open(&mut open_attempt, Vec::new())
            .expect("finish_open");
        drop(open_attempt);

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(threads);
        let mut start_tx = Vec::with_capacity(threads);
        let mut workers = Vec::with_capacity(threads);

        for _ in 0..threads {
            let (s_tx, s_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let d_tx = done_tx.clone();
            start_tx.push(s_tx);

            let r = runtime;
            let handle = std::thread::spawn(move || {
                let _subscriber_guard = matches!(kind, SyncBenchKind::ScalarReturnUdfTraceEnabled)
                    .then(install_benchmark_subscriber);

                while s_rx.recv().is_ok() {
                    match kind {
                        SyncBenchKind::IngressUdfOnly => {
                            for _ in 0..iterations_per_thread {
                                let (guard, accepted) =
                                    crate::ingress::global_ingress().enter_udf_with(|| {});
                                std::hint::black_box(accepted);
                                drop(guard);
                            }
                        }
                        SyncBenchKind::FullAdmission => {
                            for _ in 0..iterations_per_thread {
                                let (guard, accepted) =
                                    crate::ingress::global_ingress().enter_udf_with(|| {});
                                if accepted && let Ok(call) = r.enter() {
                                    std::hint::black_box(&call);
                                }
                                drop(guard);
                            }
                        }
                        SyncBenchKind::ScalarReturnNoSubscriber
                        | SyncBenchKind::ScalarReturnUdfTraceEnabled => {
                            for _ in 0..iterations_per_thread {
                                let ptr = crate::return_value::udf_boundary_named(
                                    r,
                                    "bench_udf",
                                    "BENCH.UDF",
                                    |_| Ok(42.0),
                                );
                                #[allow(
                                    unsafe_code,
                                    reason = "Internal benchmark resource cleanup"
                                )]
                                // SAFETY: ptr is a valid return block pointer produced by udf_boundary_named for this benchmark.
                                unsafe {
                                    let _ = crate::return_value::free_return_boundary(ptr);
                                }
                            }
                        }
                        SyncBenchKind::ReturnTrackerOnly => {
                            for _ in 0..iterations_per_thread {
                                let producer = r
                                    .enter_return_producer()
                                    .expect("return admission must be open for benchmark");
                                std::hint::black_box(&producer);
                                drop(producer);
                            }
                        }
                    }
                    d_tx.send(()).unwrap();
                }
            });
            workers.push(handle);
        }

        Self {
            _runtime: runtime,
            threads,
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run_batch(&self) {
        for tx in &self.start_tx {
            tx.send(()).expect("worker thread received start signal");
        }
        for _ in 0..self.threads {
            self.done_rx
                .recv()
                .expect("worker thread finished batch processing");
        }
    }
}

impl Drop for SyncBoundaryWorkerPool {
    fn drop(&mut self) {
        // Drop senders so workers exit their loops
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        if matches!(
            crate::ingress::global_ingress().phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            crate::ingress::global_ingress().begin_close_with(|| {});
            let _ = crate::ingress::global_ingress().seal_and_drain();
        }
    }
}

// ---------------------------------------------------------------------------
// Handle prepare benchmarks
// ---------------------------------------------------------------------------

use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaRevisionKey, HandleRuntime, HandleTopicKey,
    resolve_formula_caller,
};
use crate::host_callback::HostCallbackSession;
use crate::input_identity::InputFingerprint;

pub struct BenchHandleObject {
    pub _payload: u64,
}
impl ExcelHandleObject for BenchHandleObject {}

fn benchmark_revision_key(udf_id: &'static str, id: u64) -> HandleTopicKey {
    let mut inputs = [0_u8; 32];
    inputs[..8].copy_from_slice(&id.to_le_bytes());
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        FormulaCaller {
            sheet_id: 1,
            row: 0,
            column: 0,
        },
        udf_id,
        InputFingerprint::from_bytes(inputs),
    ))
}

fn cleanup_handle_runtime(runtime: &HandleRuntime) {
    runtime.terminate_all_topics();
    let _ = runtime.close();
}

/// A batch whose runtime and formula keys are prepared before the timed call.
pub struct HandleColdBatch {
    runtime: Arc<HandleRuntime>,
    keys: Vec<HandleTopicKey>,
}

impl HandleColdBatch {
    pub fn new(iterations: usize) -> Self {
        Self {
            runtime: Arc::new(
                HandleRuntime::try_new_with_ingress(iterations.max(1), None)
                    .expect("benchmark host provides an OS CSPRNG"),
            ),
            keys: (0..iterations)
                .map(|i| benchmark_revision_key("BENCH.COLD", i as u64))
                .collect(),
        }
    }

    pub fn run(&mut self) {
        let keys = std::mem::take(&mut self.keys);
        for (i, key) in keys.into_iter().enumerate() {
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("cold handle publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleColdBatch {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A warm-hit benchmark with its seed publication outside the timed section.
pub struct HandleWarmBenchmark {
    runtime: Arc<HandleRuntime>,
    key: HandleTopicKey,
}

impl HandleWarmBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(1, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let key = benchmark_revision_key("BENCH.WARM", 0);
        runtime
            .prepare_observed(key, || Ok(BenchHandleObject { _payload: 0 }), |_, _| Ok(()))
            .expect("warm handle seed publication failed");
        Self { runtime, key }
    }

    pub fn run(&self, iterations: usize) {
        for _ in 0..iterations {
            let result = self
                .runtime
                .prepare_observed(
                    self.key,
                    || -> crate::XllResult<BenchHandleObject> {
                        unreachable!("warm factory must not run")
                    },
                    |_, _| Ok(()),
                )
                .expect("warm handle observation failed");
            std::hint::black_box(result);
        }
    }
}

impl Default for HandleWarmBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HandleWarmBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A cold-growth benchmark that inserts `N` unique topic keys into a single runtime.
pub struct HandleColdGrowthBenchmark {
    runtime: Arc<HandleRuntime>,
    keys: Vec<HandleTopicKey>,
}

impl HandleColdGrowthBenchmark {
    pub fn new(count: usize) -> Self {
        Self {
            runtime: Arc::new(
                HandleRuntime::try_new_with_ingress(count.max(1), None)
                    .expect("benchmark host provides an OS CSPRNG"),
            ),
            keys: (0..count)
                .map(|i| benchmark_revision_key("BENCH.COLD_GROW", i as u64))
                .collect(),
        }
    }

    pub fn run(&self) {
        for (i, &key) in self.keys.iter().enumerate() {
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("cold handle growth publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleColdGrowthBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A revision-churn benchmark that repeatedly updates the same `N` topics with new objects.
pub struct HandleRevisionChurnBenchmark {
    runtime: Arc<HandleRuntime>,
    keys: Vec<HandleTopicKey>,
    churn_cycles: usize,
}

impl HandleRevisionChurnBenchmark {
    pub fn new(topics: usize, churn_cycles: usize) -> Self {
        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(topics.max(1), None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let keys: Vec<_> = (0..topics)
            .map(|i| benchmark_revision_key("BENCH.CHURN", i as u64))
            .collect();
        for (i, &key) in keys.iter().enumerate() {
            runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("initial seed publication failed");
        }
        Self {
            runtime,
            keys,
            churn_cycles,
        }
    }

    pub fn run(&self) {
        for cycle in 0..self.churn_cycles {
            let key = self.keys[cycle % self.keys.len()];
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || {
                        Ok(BenchHandleObject {
                            _payload: cycle as u64,
                        })
                    },
                    |_, _| Ok(()),
                )
                .expect("revision churn publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleRevisionChurnBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

// ---------------------------------------------------------------------------
// Formula-to-handle end-to-end benchmarks
// ---------------------------------------------------------------------------

const HANDLE_FORMULA_UDF_ID: &str = "BENCH.HANDLE";

fn fingerprint_argument<T>(value: &T) -> [u8; 32]
where
    T: for<'call> ExcelParameter<'call>,
{
    let mut builder = crate::input_identity::InputFingerprintBuilder::new();
    builder
        .with_argument("benchmark", |encoder| {
            value.encode_identity(encoder);
            Ok(())
        })
        .expect("benchmark semantic argument must fingerprint successfully");
    *builder.finish().as_bytes()
}

pub struct SemanticIdentityBenchmark<T> {
    value: T,
}

impl<T> SemanticIdentityBenchmark<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }

    pub fn run(&self) -> [u8; 32]
    where
        T: for<'call> ExcelParameter<'call>,
    {
        fingerprint_argument(&self.value)
    }
}

pub struct FormulaRevisionBenchmark<T> {
    runtime: Arc<HandleRuntime>,
    argument: T,
    caller: FormulaCaller,
    factory_calls: AtomicUsize,
}

impl<T> FormulaRevisionBenchmark<T>
where
    T: for<'call> ExcelParameter<'call>,
{
    pub fn new(argument: T) -> Self {
        let caller = FormulaCaller {
            sheet_id: 7,
            row: 42,
            column: 11,
        };
        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(1, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let factory_calls = AtomicUsize::new(0);
        let key = formula_revision_key(&argument, caller);

        runtime
            .prepare_observed(
                key,
                || {
                    factory_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(BenchHandleObject { _payload: 0 })
                },
                |_, _| Ok(()),
            )
            .expect("formula handle warm seed publication failed");
        assert_eq!(factory_calls.load(Ordering::Relaxed), 1);

        Self {
            runtime,
            argument,
            caller,
            factory_calls,
        }
    }

    pub fn run(&self) -> (String, bool) {
        let key = formula_revision_key(&self.argument, self.caller);
        self.runtime
            .prepare_observed(
                key,
                || -> crate::XllResult<BenchHandleObject> {
                    self.factory_calls.fetch_add(1, Ordering::Relaxed);
                    panic!("formula handle warm-hit factory must not run");
                },
                |_, _| Ok(()),
            )
            .expect("formula handle warm observation failed")
    }

    pub fn assert_warm_hit(&self) {
        assert_eq!(
            self.factory_calls.load(Ordering::Relaxed),
            1,
            "formula handle benchmark executed its factory during warm-hit measurement"
        );
    }
}

fn formula_revision_key<T>(arguments: &T, caller: FormulaCaller) -> HandleTopicKey
where
    T: for<'call> ExcelParameter<'call>,
{
    let inputs = fingerprint_argument(arguments);
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        caller,
        HANDLE_FORMULA_UDF_ID,
        InputFingerprint::from_bytes(inputs),
    ))
}

impl<T> Drop for FormulaRevisionBenchmark<T> {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

// ---------------------------------------------------------------------------
// Formula caller resolution benchmarks
// ---------------------------------------------------------------------------

const BENCH_CALLER_REF: u8 = 1;
const BENCH_CALLER_SREF: u8 = 2;
static BENCH_CALLER_KIND: AtomicU8 = AtomicU8::new(BENCH_CALLER_REF);
static BENCH_CALLER_REFERENCES: xlfn_sys::XLMREF12 = xlfn_sys::XLMREF12 {
    count: 1,
    reftbl: [xlfn_sys::XLREF12 {
        rw_first: 11,
        rw_last: 11,
        col_first: 3,
        col_last: 3,
    }],
};
static BENCH_SHEET_NAME: [u16; 6] = [
    5,
    b'S' as u16,
    b'h' as u16,
    b'e' as u16,
    b'e' as u16,
    b't' as u16,
];

#[derive(Clone, Copy, Debug)]
pub enum FormulaCallerBenchCase {
    Ref,
    SRef,
}

impl FormulaCallerBenchCase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ref => "ref",
            Self::SRef => "sref",
        }
    }

    const fn raw(self) -> u8 {
        match self {
            Self::Ref => BENCH_CALLER_REF,
            Self::SRef => BENCH_CALLER_SREF,
        }
    }
}

/// A standalone callback stub that preserves the production callback and
/// release sequence while making the host-side Excel work deterministic.
unsafe extern "system" fn benchmark_formula_callback(
    function: i32,
    _argument_count: i32,
    _arguments: *mut *mut xlfn_sys::XLOPER12,
    result: *mut xlfn_sys::XLOPER12,
) -> i32 {
    use xlfn_sys::{
        XL_FREE, XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLRET_FAILED, XLRET_SUCCESS, XLTYPE_REF,
        XLTYPE_SREF, XLTYPE_STR,
    };

    if function == XL_FREE {
        return XLRET_SUCCESS;
    }
    if result.is_null() {
        return XLRET_FAILED;
    }

    let references = (&BENCH_CALLER_REFERENCES as *const xlfn_sys::XLMREF12).cast_mut();
    let value = match function {
        XLF_CALLER if BENCH_CALLER_KIND.load(Ordering::Relaxed) == BENCH_CALLER_REF => {
            xlfn_sys::XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    mref: xlfn_sys::XLOPER12MRef {
                        references,
                        sheet_id: 17,
                    },
                },
                xltype: XLTYPE_REF,
            }
        }
        XLF_CALLER => xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                sref: xlfn_sys::XLOPER12SRef {
                    count: 1,
                    reference: xlfn_sys::XLREF12 {
                        rw_first: 11,
                        rw_last: 11,
                        col_first: 3,
                        col_last: 3,
                    },
                },
            },
            xltype: XLTYPE_SREF,
        },
        XL_SHEET_NM => xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: BENCH_SHEET_NAME.as_ptr().cast_mut(),
            },
            xltype: XLTYPE_STR,
        },
        XL_SHEET_ID => xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                mref: xlfn_sys::XLOPER12MRef {
                    references,
                    sheet_id: 19,
                },
            },
            xltype: XLTYPE_REF,
        },
        _ => return XLRET_FAILED,
    };

    // SAFETY: the callback contract supplies writable result storage for every
    // non-release function handled above.
    unsafe {
        *result = value;
    }
    XLRET_SUCCESS
}

pub struct FormulaCallerBenchmark {
    callbacks: HostCallbackSession,
}

impl FormulaCallerBenchmark {
    pub fn new(case: FormulaCallerBenchCase) -> Self {
        BENCH_CALLER_KIND.store(case.raw(), Ordering::Relaxed);
        crate::callback_gate::reset();
        // SAFETY: `benchmark_formula_callback` has Excel's exact callback ABI
        // and remains live for the duration of this benchmark process.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                benchmark_formula_callback as *const () as *mut std::ffi::c_void,
            );
        }

        let callbacks = HostCallbackSession::new();
        let caller = resolve_formula_caller(&callbacks)
            .expect("benchmark callback must resolve a single-cell caller");
        let expected_sheet = if matches!(case, FormulaCallerBenchCase::Ref) {
            17
        } else {
            19
        };
        assert_eq!(caller.sheet_id, expected_sheet);
        assert_eq!((caller.row, caller.column), (11, 3));

        Self { callbacks }
    }

    pub fn run(&self) -> (usize, i32, i32) {
        let caller = resolve_formula_caller(&self.callbacks)
            .expect("benchmark callback must resolve a single-cell caller");
        (caller.sheet_id, caller.row, caller.column)
    }
}

// ---------------------------------------------------------------------------
// Handle lookup benchmarks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum HandleLookupBenchCase {
    WarmSameToken,
    DistinctTokens,
}

impl HandleLookupBenchCase {
    pub const ALL: [Self; 2] = [Self::WarmSameToken, Self::DistinctTokens];

    pub const fn name(self) -> &'static str {
        match self {
            Self::WarmSameToken => "warm_same_token",
            Self::DistinctTokens => "distinct_tokens",
        }
    }
}

pub struct HandleLookupBenchmark {
    runtime: Arc<HandleRuntime>,
    worker_count: usize,
    iterations_per_worker: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HandleLookupBenchmark {
    pub fn new(
        case: HandleLookupBenchCase,
        worker_count: usize,
        iterations_per_worker: usize,
    ) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(worker_count, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let mut tokens = Vec::with_capacity(worker_count);
        for worker in 0..worker_count {
            let key = benchmark_revision_key("BENCH.LOOKUP", worker as u64);
            let token = runtime
                .prepare_observed(key, || Ok(BenchHandleObject { _payload: 0 }), |_, _| Ok(()))
                .expect("handle lookup warm seed publication failed")
                .0;
            tokens.push(Arc::<str>::from(token));
        }

        match case {
            HandleLookupBenchCase::WarmSameToken => tokens.truncate(1),
            HandleLookupBenchCase::DistinctTokens => {}
        }

        let tokens = Arc::new(tokens);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for worker in 0..worker_count {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            let worker_runtime = Arc::clone(&runtime);
            let worker_tokens = Arc::clone(&tokens);
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while worker_rx.recv().is_ok() {
                    let token_index = worker.min(worker_tokens.len() - 1);
                    let token = worker_tokens[token_index].as_ref();
                    for _ in 0..iterations_per_worker {
                        let result = crate::with_excel_call_scope(|scope| {
                            worker_runtime
                                .lookup::<BenchHandleObject>(scope, token)
                                .map(|handle| {
                                    std::hint::black_box(&*handle);
                                })
                        });
                        let _ = std::hint::black_box(result);
                    }
                    done_tx
                        .send(())
                        .expect("handle lookup benchmark driver received completion signal");
                }
            }));
        }

        Self {
            runtime,
            worker_count,
            iterations_per_worker,
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run(&self) {
        for start in &self.start_tx {
            start
                .send(())
                .expect("handle lookup benchmark worker received start signal");
        }
        for _ in 0..self.worker_count {
            self.done_rx
                .recv()
                .expect("handle lookup benchmark worker finished batch");
        }
    }

    pub fn total_iterations(&self) -> usize {
        self.worker_count * self.iterations_per_worker
    }
}

impl Drop for HandleLookupBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A persistent worker pool that looks up a different warm topic per worker.
///
/// The topics, worker threads, and synchronization channels are all prepared
/// before measurement. Each timed batch therefore exercises concurrent
/// `TopicState` access without paying for cold publication or thread creation.
pub struct HandleDistinctKeyBenchmark {
    runtime: Arc<HandleRuntime>,
    iterations_per_worker: usize,
    factory_calls: Arc<AtomicUsize>,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HandleDistinctKeyBenchmark {
    pub fn new(worker_count: usize, iterations_per_worker: usize) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(worker_count, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let keys = (0..worker_count)
            .map(|worker| benchmark_revision_key("BENCH.DISTINCT", worker as u64))
            .collect::<Vec<_>>();

        for key in &keys {
            let factory_calls = Arc::clone(&factory_calls);
            runtime
                .prepare_observed(
                    *key,
                    move || {
                        factory_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(BenchHandleObject { _payload: 0 })
                    },
                    |_, _| Ok(()),
                )
                .expect("distinct-key warm seed publication failed");
        }

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for key in keys {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            let worker_runtime = Arc::clone(&runtime);
            let worker_factory_calls = Arc::clone(&factory_calls);
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while worker_rx.recv().is_ok() {
                    for _ in 0..iterations_per_worker {
                        let result = worker_runtime
                            .prepare_observed(
                                key,
                                || {
                                    worker_factory_calls.fetch_add(1, Ordering::Relaxed);
                                    Ok(BenchHandleObject { _payload: 0 })
                                },
                                |_, _| Ok(()),
                            )
                            .expect("distinct-key warm observation failed");
                        std::hint::black_box(result);
                    }
                    done_tx
                        .send(())
                        .expect("benchmark driver received completion signal");
                }
            }));
        }

        Self {
            runtime,
            iterations_per_worker,
            factory_calls,
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run(&self) {
        for start in &self.start_tx {
            start
                .send(())
                .expect("distinct-key benchmark worker received start signal");
        }
        for _ in 0..self.start_tx.len() {
            self.done_rx
                .recv()
                .expect("distinct-key benchmark worker finished batch");
        }
    }

    pub fn total_iterations(&self) -> usize {
        self.start_tx.len() * self.iterations_per_worker
    }

    pub fn assert_warm_hit(&self) {
        assert_eq!(
            self.factory_calls.load(Ordering::Relaxed),
            self.start_tx.len(),
            "distinct-key benchmark executed a factory during warm-hit measurement"
        );
    }
}

impl Drop for HandleDistinctKeyBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        cleanup_handle_runtime(&self.runtime);
    }
}

fn get_benchmark_runtime() -> &'static crate::Runtime<()> {
    static RUNTIME: std::sync::OnceLock<crate::Runtime<()>> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(crate::Runtime::new)
}

/// Benchmark harness for measuring raw Excel argument ingress conversion costs
/// with and without semantic identity fingerprinting.
pub struct RawArgumentIngressBenchmark {
    runtime: &'static crate::Runtime<()>,
    handle_runtime: Option<Arc<HandleRuntime>>,
    raw: xlfn_sys::XLOPER12,
    _storage: Option<Box<dyn std::any::Any>>,
}

impl RawArgumentIngressBenchmark {
    pub fn number(value: f64) -> Self {
        Self {
            runtime: get_benchmark_runtime(),
            handle_runtime: None,
            raw: xlfn_sys::XLOPER12::number(value),
            _storage: None,
        }
    }

    pub fn string(value: &str) -> Self {
        let mut u16_chars: Vec<u16> = Vec::with_capacity(value.len() + 1);
        u16_chars.push(value.len() as u16);
        u16_chars.extend(value.encode_utf16());
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: u16_chars.as_ptr() as *mut u16,
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        Self {
            runtime: get_benchmark_runtime(),
            handle_runtime: None,
            raw,
            _storage: Some(Box::new(u16_chars)),
        }
    }

    pub fn number_matrix(rows: usize, columns: usize) -> Self {
        let len = rows * columns;
        let mut cells = (0..len)
            .map(|i| xlfn_sys::XLOPER12::number(i as f64))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: rows as i32,
                    columns: columns as i32,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        Self {
            runtime: get_benchmark_runtime(),
            handle_runtime: None,
            raw,
            _storage: Some(Box::new(cells)),
        }
    }

    pub fn number_vec(len: usize) -> Self {
        // Excel worksheet rows support up to 1,048,576 elements while columns support up to 16,384.
        // We use an N x 1 column vector representation so 100k+ element 1D vectors fit within Excel dimensions.
        Self::number_matrix(len, 1)
    }

    pub fn handle() -> Self {
        let runtime = get_benchmark_runtime();
        let handle_runtime = runtime
            .handles()
            .expect("benchmark handle runtime must initialize");
        let key = benchmark_revision_key("BENCH.INGRESS.HANDLE", 1);
        let token = handle_runtime
            .prepare_observed(
                key,
                || Ok(BenchHandleObject { _payload: 42 }),
                |_, _| Ok(()),
            )
            .expect("benchmark handle preparation must succeed")
            .0;
        let mut u16_chars: Vec<u16> = Vec::with_capacity(token.len() + 1);
        u16_chars.push(token.len() as u16);
        u16_chars.extend(token.encode_utf16());
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: u16_chars.as_ptr() as *mut u16,
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        Self {
            runtime,
            handle_runtime: Some(handle_runtime),
            raw,
            _storage: Some(Box::new(u16_chars)),
        }
    }

    pub fn run_plain<T>(&mut self)
    where
        T: for<'call> ExcelParameter<'call>,
    {
        crate::with_excel_call_scope(|scope| {
            let mut arguments =
                crate::value::ArgumentContext::for_return::<f64, _>(self.runtime, scope);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<T>(
                    &mut arguments,
                    "arg",
                    &mut self.raw,
                )
            }
            .expect("benchmark raw argument ingress must succeed");
            std::hint::black_box(&value);
            let _ = arguments.finish();
        })
    }

    pub fn run_with_identity<T>(&mut self) -> [u8; 32]
    where
        T: for<'call> ExcelParameter<'call>,
    {
        crate::with_excel_call_scope(|scope| {
            let mut arguments = crate::value::ArgumentContext::for_return::<
                crate::HandleAlias<'static, BenchHandleObject>,
                _,
            >(self.runtime, scope);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<T>(
                    &mut arguments,
                    "arg",
                    &mut self.raw,
                )
            }
            .expect("benchmark raw argument ingress with identity must succeed");
            std::hint::black_box(&value);
            arguments
                .finish()
                .expect("formula revision return must produce fingerprint")
        })
    }

    pub fn run_handle_plain<T>(&mut self)
    where
        T: ExcelHandleObject,
    {
        crate::with_excel_call_scope(|scope| {
            let mut arguments =
                crate::value::ArgumentContext::for_return::<f64, _>(self.runtime, scope);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<crate::Handle<'_, T>>(
                    &mut arguments,
                    "arg",
                    &mut self.raw,
                )
            }
            .expect("benchmark raw handle ingress must succeed");
            std::hint::black_box(&value);
            let _ = arguments.finish();
        })
    }

    pub fn run_handle_with_identity<T>(&mut self) -> [u8; 32]
    where
        T: ExcelHandleObject,
    {
        crate::with_excel_call_scope(|scope| {
            let mut arguments = crate::value::ArgumentContext::for_return::<
                crate::HandleAlias<'static, BenchHandleObject>,
                _,
            >(self.runtime, scope);
            // SAFETY: self.raw points to valid benchmark storage that remains live.
            let value = unsafe {
                crate::value::argument_from_raw_with_arguments::<crate::Handle<'_, T>>(
                    &mut arguments,
                    "arg",
                    &mut self.raw,
                )
            }
            .expect("benchmark raw handle ingress with identity must succeed");
            std::hint::black_box(&value);
            arguments
                .finish()
                .expect("formula revision return must produce fingerprint")
        })
    }
}

impl Drop for RawArgumentIngressBenchmark {
    fn drop(&mut self) {
        if let Some(handle_rt) = &self.handle_runtime {
            cleanup_handle_runtime(handle_rt);
        }
    }
}

pub struct MultiHandleCallBenchmark {
    runtime: &'static crate::Runtime<()>,
    _handle_runtime: Arc<HandleRuntime>,
    raw_tokens: Vec<xlfn_sys::XLOPER12>,
    _storage: Vec<Vec<u16>>,
}

impl MultiHandleCallBenchmark {
    pub fn new(count: usize) -> Self {
        let runtime = get_benchmark_runtime();
        let handle_runtime = runtime
            .handles()
            .expect("benchmark handle runtime must initialize");
        let mut raw_tokens = Vec::with_capacity(count);
        let mut storage = Vec::with_capacity(count);
        for i in 0..count {
            let key = benchmark_revision_key("BENCH.MULTI.HANDLE", i as u64);
            let token = handle_runtime
                .prepare_observed(
                    key,
                    move || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("benchmark handle preparation must succeed")
                .0;
            let mut u16_chars: Vec<u16> = Vec::with_capacity(token.len() + 1);
            u16_chars.push(token.len() as u16);
            u16_chars.extend(token.encode_utf16());
            let raw = xlfn_sys::XLOPER12 {
                value: xlfn_sys::XLOPER12Value {
                    string: u16_chars.as_ptr() as *mut u16,
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
            raw_tokens.push(raw);
            storage.push(u16_chars);
        }
        Self {
            runtime,
            _handle_runtime: handle_runtime,
            raw_tokens,
            _storage: storage,
        }
    }

    pub fn run(&mut self) {
        crate::with_excel_call_scope(|scope| {
            let mut frame = crate::macro_support::CallFrame::new::<f64, _>(self.runtime, scope);
            for raw in &mut self.raw_tokens {
                // SAFETY: raw points to valid benchmark storage.
                let handle: crate::Handle<'_, BenchHandleObject> = unsafe {
                    frame
                        .convert_argument("arg", raw)
                        .expect("benchmark argument conversion must succeed")
                };
                std::hint::black_box(handle);
            }
            let return_ctx = frame.return_context("bench_udf");
            std::hint::black_box(return_ctx);
        });
    }
}

pub struct ConcurrentHandleResolutionBenchmark {
    _slot: &'static crate::handle::HandleRuntimeSlot,
    threads: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl ConcurrentHandleResolutionBenchmark {
    pub fn new(threads: usize, iterations_per_thread: usize) -> Self {
        let runtime = get_benchmark_runtime();
        let _ = runtime
            .handles()
            .expect("benchmark handle runtime must initialize");
        let slot = runtime.handle_runtime_slot();

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(threads);
        let mut start_tx = Vec::with_capacity(threads);
        let mut workers = Vec::with_capacity(threads);

        for _ in 0..threads {
            let (s_tx, s_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let d_tx = done_tx.clone();
            start_tx.push(s_tx);

            let handle = std::thread::spawn(move || {
                while s_rx.recv().is_ok() {
                    for _ in 0..iterations_per_thread {
                        let resolver = crate::handle::HandleRuntimeResolver::new(slot);
                        let rt = resolver.get().expect("handle runtime must resolve");
                        std::hint::black_box(rt);
                    }
                    let _ = d_tx.send(());
                }
            });
            workers.push(handle);
        }

        Self {
            _slot: slot,
            threads,
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run_batch(&self) {
        for tx in &self.start_tx {
            let _ = tx.send(());
        }
        for _ in 0..self.threads {
            let _ = self.done_rx.recv();
        }
    }
}

impl Drop for ConcurrentHandleResolutionBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

struct BenchmarkSubscription;

// SAFETY: benchmark dummy subscription is thread-safe and has no resources.
unsafe impl crate::RtdSubscription for BenchmarkSubscription {
    fn request_cancel(&self) {}
    fn disconnect_and_wait(self: Box<Self>) -> crate::XllResult<()> {
        Ok(())
    }
}

struct BenchmarkRtdSource<T> {
    sink: parking_lot::Mutex<Option<crate::RtdSink<T>>>,
}

impl<T: crate::IntoRtdValue + Clone + Send + Sync + 'static> crate::RtdSource
    for BenchmarkRtdSource<T>
{
    type Value = T;
    type Subscription = BenchmarkSubscription;

    fn subscribe(
        &self,
        _topic: &crate::RtdTopic,
        sink: crate::RtdSink<Self::Value>,
    ) -> crate::XllResult<Self::Subscription> {
        *self.sink.lock() = Some(sink);
        Ok(BenchmarkSubscription)
    }
}

pub struct RtdPublishNumberBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::RtdServerHandle,
    sink: crate::RtdSink<f64>,
}

impl Default for RtdPublishNumberBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishNumberBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(crate::subscription::ServerGeneration(1))
            .expect("server registration must succeed");
        let source = Arc::new(BenchmarkRtdSource {
            sink: parking_lot::Mutex::new(None),
        });
        let topic =
            crate::RtdTopic::new(["BENCH", "NUMBER"]).expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(Arc::clone(&source), topic)
            .expect("prepare must succeed");
        let key = prepared.key().clone();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(1), &key)
            .expect("connect_transaction must succeed");
        conn.commit().expect("connection commit must succeed");
        prepared.commit();
        let sink = source.sink.lock().clone().expect("sink must be captured");
        Self {
            _runtime: runtime,
            server,
            sink,
        }
    }

    #[inline]
    pub fn run_coalesced(&self, iterations: usize) {
        for i in 0..iterations {
            self.sink
                .publish(12.5 + i as f64)
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_drain_each(&self, iterations: usize) {
        for i in 0..iterations {
            self.sink
                .publish(12.5 + i as f64)
                .expect("publish must succeed");
            let batch = self
                .server
                .begin_refresh()
                .expect("begin_refresh must succeed");
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .expect("complete must succeed");
        }
    }
}

pub struct RtdPublishStringBenchmark {
    _runtime: Arc<crate::subscription::SubscriptionRuntime>,
    server: crate::subscription::RtdServerHandle,
    sink: crate::RtdSink<String>,
}

impl Default for RtdPublishStringBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl RtdPublishStringBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(crate::subscription::SubscriptionRuntime::new());
        let server = runtime
            .register_server(crate::subscription::ServerGeneration(1))
            .expect("server registration must succeed");
        let source = Arc::new(BenchmarkRtdSource {
            sink: parking_lot::Mutex::new(None),
        });
        let topic =
            crate::RtdTopic::new(["BENCH", "STRING"]).expect("benchmark RTD topic must be valid");
        let prepared = runtime
            .prepare(Arc::clone(&source), topic)
            .expect("prepare must succeed");
        let key = prepared.key().clone();
        let conn = runtime
            .connect_transaction(&server, crate::subscription::TopicId(2), &key)
            .expect("connect_transaction must succeed");
        conn.commit().expect("connection commit must succeed");
        prepared.commit();
        let sink = source.sink.lock().clone().expect("sink must be captured");
        Self {
            _runtime: runtime,
            server,
            sink,
        }
    }

    #[inline]
    pub fn run_coalesced(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
        }
    }

    #[inline]
    pub fn run_drain_each(&self, iterations: usize) {
        for _ in 0..iterations {
            self.sink
                .publish("stream_market_data_update_payload".to_owned())
                .expect("publish must succeed");
            let batch = self
                .server
                .begin_refresh()
                .expect("begin_refresh must succeed");
            batch
                .complete(crate::subscription::RefreshOutcome::Delivered)
                .expect("complete must succeed");
        }
    }
}
