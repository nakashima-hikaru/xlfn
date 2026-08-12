//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]
#![allow(unsafe_code, reason = "Benchmark-only XLOPER12 pointer construction")]

#[cfg(feature = "async")]
use crate::async_udf::AsyncManager;
#[cfg(feature = "async")]
use crate::cancellation::CancellationSource;
#[cfg(feature = "async")]
use crate::{CancellationGuarantee, XllError};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    AdmissionOnly,
    ScalarReturn,
}

pub struct SyncBoundaryWorkerPool {
    _runtime: &'static crate::Runtime<()>,
    threads: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<SyncBenchKind>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl SyncBoundaryWorkerPool {
    pub fn new(threads: usize, iterations_per_thread: usize) -> Self {
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
        runtime.publish_state(());
        runtime
            .finish_open(&mut open_attempt, Vec::new())
            .expect("finish_open");

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(threads);
        let mut start_tx = Vec::with_capacity(threads);
        let mut workers = Vec::with_capacity(threads);

        for _ in 0..threads {
            let (s_tx, s_rx) = std::sync::mpsc::sync_channel::<SyncBenchKind>(1);
            let d_tx = done_tx.clone();
            start_tx.push(s_tx);

            let r = runtime;
            let handle = std::thread::spawn(move || {
                while let Ok(kind) = s_rx.recv() {
                    match kind {
                        SyncBenchKind::AdmissionOnly => {
                            for _ in 0..iterations_per_thread {
                                let (_guard, accepted, _concurrent_calls) =
                                    crate::ingress::global_ingress().enter_udf_with(|| {});
                                if accepted && let Ok(call) = r.enter() {
                                    std::hint::black_box(&call);
                                }
                            }
                        }
                        SyncBenchKind::ScalarReturn => {
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

    pub fn run_batch(&self, kind: SyncBenchKind) {
        for tx in &self.start_tx {
            tx.send(kind).expect("worker thread received start signal");
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
// Formula fingerprint benchmarks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum FingerprintBenchCase {
    ScalarInteger,
    ShortString,
    Utf16String32KiB,
    NumericCells10K,
    NumericCells100K,
}

impl FingerprintBenchCase {
    pub const ALL: [Self; 5] = [
        Self::ScalarInteger,
        Self::ShortString,
        Self::Utf16String32KiB,
        Self::NumericCells10K,
        Self::NumericCells100K,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ScalarInteger => "scalar_integer",
            Self::ShortString => "short_string",
            Self::Utf16String32KiB => "utf16_string_32kib",
            Self::NumericCells10K => "numeric_cells_10k",
            Self::NumericCells100K => "numeric_cells_100k",
        }
    }
}

pub struct FingerprintBenchmark {
    payload: Vec<u8>,
    chunks: Vec<std::ops::Range<usize>>,
    use_buffer: bool,
}

impl FingerprintBenchmark {
    pub fn new(case: FingerprintBenchCase) -> Self {
        let mut benchmark = Self {
            payload: Vec::new(),
            chunks: Vec::new(),
            use_buffer: matches!(
                case,
                FingerprintBenchCase::Utf16String32KiB
                    | FingerprintBenchCase::NumericCells10K
                    | FingerprintBenchCase::NumericCells100K
            ),
        };

        benchmark.push_chunk(b"xlfn-formula-fingerprint-v1\0");
        benchmark.push_u64(1);

        match case {
            FingerprintBenchCase::ScalarInteger => {
                benchmark.push_u8(3);
                benchmark.push_u32(0);
            }
            FingerprintBenchCase::ShortString => benchmark.push_string(5),
            FingerprintBenchCase::Utf16String32KiB => benchmark.push_string(16 * 1024),
            FingerprintBenchCase::NumericCells10K => benchmark.push_numeric_array(10_000),
            FingerprintBenchCase::NumericCells100K => benchmark.push_numeric_array(100_000),
        }

        benchmark
    }

    pub fn encoded_bytes(&self) -> usize {
        self.payload.len()
    }

    pub fn run_buffered(&self) -> [u8; 32] {
        crate::formula_fingerprint::benchmark_stream(&self.payload, &self.chunks)
    }

    pub fn run_selected(&self) -> [u8; 32] {
        if self.use_buffer {
            self.run_buffered()
        } else {
            self.run_direct()
        }
    }

    pub fn run_direct(&self) -> [u8; 32] {
        crate::formula_fingerprint::benchmark_direct_stream(&self.payload, &self.chunks)
    }

    fn push_chunk(&mut self, bytes: &[u8]) {
        let start = self.payload.len();
        self.payload.extend_from_slice(bytes);
        self.chunks.push(start..self.payload.len());
    }

    fn push_u8(&mut self, value: u8) {
        self.push_chunk(&[value]);
    }

    fn push_u16(&mut self, value: u16) {
        self.push_chunk(&value.to_le_bytes());
    }

    fn push_u32(&mut self, value: u32) {
        self.push_chunk(&value.to_le_bytes());
    }

    fn push_u64(&mut self, value: u64) {
        self.push_chunk(&value.to_le_bytes());
    }

    fn push_string(&mut self, units: usize) {
        self.push_u8(4);
        self.push_u64(units as u64);
        for index in 0..units {
            self.push_u16((b'a' + (index % 26) as u8) as u16);
        }
    }

    fn push_numeric_array(&mut self, cells: usize) {
        self.push_u8(8);
        self.push_u64(cells as u64);
        self.push_u64(1);
        for index in 0..cells {
            self.push_u8(1);
            self.push_u64((index as f64).to_bits());
        }
    }
}

// ---------------------------------------------------------------------------
// Handle prepare benchmarks
// ---------------------------------------------------------------------------

use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaTopicKey, HandleRuntime, HandleTopicKey,
};
use xlfn_sys::{XLOPER12, XLOPER12Array, XLOPER12Value, XLTYPE_MULTI, XLTYPE_STR};

struct BenchHandleObject {
    _payload: u64,
}
impl ExcelHandleObject for BenchHandleObject {}

const HANDLE_CONTENTION_WORKERS: usize = 4;
// Contention uses one fresh topic per benchmark iteration. Keep the logical
// limit at the registry's token-slot maximum so Criterion cannot exhaust it
// during a longer default run; this does not preallocate the slot storage.
const HANDLE_CONTENTION_CAPACITY: usize = u32::MAX as usize;

fn benchmark_topic_key(udf_id: &'static str, id: u64) -> HandleTopicKey {
    let mut digest = [0_u8; 32];
    digest[..8].copy_from_slice(&id.to_le_bytes());
    HandleTopicKey::Formula(FormulaTopicKey::new(
        FormulaCaller {
            sheet_id: 1,
            row: 0,
            column: 0,
        },
        udf_id,
        &digest,
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
                .map(|i| benchmark_topic_key("BENCH.COLD", i as u64))
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
                    || Ok(Arc::new(BenchHandleObject { _payload: i as u64 })),
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
        let key = benchmark_topic_key("BENCH.WARM", 0);
        runtime
            .prepare_observed(
                key,
                || Ok(Arc::new(BenchHandleObject { _payload: 0 })),
                |_, _| Ok(()),
            )
            .expect("warm handle seed publication failed");
        Self { runtime, key }
    }

    pub fn run(&self, iterations: usize) {
        for _ in 0..iterations {
            let result = self
                .runtime
                .prepare_observed(
                    self.key,
                    || -> crate::XllResult<Arc<BenchHandleObject>> {
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

// ---------------------------------------------------------------------------
// Formula-to-handle end-to-end benchmarks
// ---------------------------------------------------------------------------

const HANDLE_FORMULA_UDF_ID: &str = "BENCH.HANDLE";

#[derive(Clone, Copy, Debug)]
pub enum HandleFormulaBenchCase {
    ScalarNumber,
    ShortString,
    NumericCells10K,
    NumericCells100K,
}

impl HandleFormulaBenchCase {
    pub const ALL: [Self; 4] = [
        Self::ScalarNumber,
        Self::ShortString,
        Self::NumericCells10K,
        Self::NumericCells100K,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ScalarNumber => "scalar_number",
            Self::ShortString => "short_string",
            Self::NumericCells10K => "numeric_cells_10k",
            Self::NumericCells100K => "numeric_cells_100k",
        }
    }
}

/// XLOPER12 arguments whose backing storage is allocated before measurement.
///
/// The raw argument pointer remains valid because the root XLOPER12 lives in a
/// Box and array/string payloads live in Vec allocations whose addresses do not
/// change when the owner is moved into the benchmark.
#[allow(
    dead_code,
    reason = "Fields intentionally keep benchmark pointers alive"
)]
struct PreparedFormulaArguments {
    root: Box<XLOPER12>,
    string_storage: Option<Vec<u16>>,
    cell_storage: Option<Vec<XLOPER12>>,
    raw_args: [*mut XLOPER12; 1],
}

impl PreparedFormulaArguments {
    fn new(case: HandleFormulaBenchCase) -> Self {
        let (root, string_storage, cell_storage) = match case {
            HandleFormulaBenchCase::ScalarNumber => (XLOPER12::number(42.0), None, None),
            HandleFormulaBenchCase::ShortString => {
                let mut storage = Vec::with_capacity(6);
                storage.push(5);
                storage.extend("short".encode_utf16());
                let root = XLOPER12 {
                    value: XLOPER12Value {
                        string: storage.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                };
                (root, Some(storage), None)
            }
            HandleFormulaBenchCase::NumericCells10K => Self::numeric_array(10_000),
            HandleFormulaBenchCase::NumericCells100K => Self::numeric_array(100_000),
        };

        let mut root = Box::new(root);
        let raw_args = [root.as_mut() as *mut XLOPER12];
        Self {
            root,
            string_storage,
            cell_storage,
            raw_args,
        }
    }

    fn numeric_array(cells: usize) -> (XLOPER12, Option<Vec<u16>>, Option<Vec<XLOPER12>>) {
        let mut storage = (0..cells)
            .map(|index| XLOPER12::number(index as f64))
            .collect::<Vec<_>>();
        let columns = cells.min(10_000);
        let rows = cells.div_ceil(columns);
        let root = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: storage.as_mut_ptr(),
                    rows: i32::try_from(rows).expect("benchmark array fits Excel rows"),
                    columns: i32::try_from(columns).expect("benchmark array fits Excel columns"),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        (root, None, Some(storage))
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        // SAFETY: `raw_args` points to the root XLOPER12 and its backing storage,
        // which remain live for the lifetime of `PreparedFormulaArguments`.
        unsafe { crate::formula_fingerprint::fingerprint(&self.raw_args) }
            .expect("benchmark XLOPER12 arguments must fingerprint successfully")
    }
}

pub struct XloperFingerprintBenchmark {
    arguments: PreparedFormulaArguments,
}

impl XloperFingerprintBenchmark {
    pub fn new(case: HandleFormulaBenchCase) -> Self {
        Self {
            arguments: PreparedFormulaArguments::new(case),
        }
    }

    pub fn run(&self) -> [u8; 32] {
        self.arguments.fingerprint()
    }
}

pub struct HandleFormulaBenchmark {
    runtime: Arc<HandleRuntime>,
    arguments: PreparedFormulaArguments,
    caller: FormulaCaller,
    factory_calls: AtomicUsize,
}

impl HandleFormulaBenchmark {
    pub fn new(case: HandleFormulaBenchCase) -> Self {
        let arguments = PreparedFormulaArguments::new(case);
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
        let key = formula_topic_key(&arguments, caller);

        runtime
            .prepare_observed(
                key,
                || {
                    factory_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(Arc::new(BenchHandleObject { _payload: 0 }))
                },
                |_, _| Ok(()),
            )
            .expect("formula handle warm seed publication failed");
        assert_eq!(factory_calls.load(Ordering::Relaxed), 1);

        Self {
            runtime,
            arguments,
            caller,
            factory_calls,
        }
    }

    pub fn run(&self) -> (String, bool) {
        let key = formula_topic_key(&self.arguments, self.caller);
        self.runtime
            .prepare_observed(
                key,
                || -> crate::XllResult<Arc<BenchHandleObject>> {
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

fn formula_topic_key(
    arguments: &PreparedFormulaArguments,
    caller: FormulaCaller,
) -> HandleTopicKey {
    // SAFETY: the root XLOPER12 and all backing storage were allocated before
    // the benchmark began and remain owned by `arguments` for this call.
    let digest = unsafe { crate::formula_fingerprint::fingerprint(&arguments.raw_args) }
        .expect("benchmark XLOPER12 arguments must fingerprint successfully");
    HandleTopicKey::Formula(FormulaTopicKey::new(caller, HANDLE_FORMULA_UDF_ID, &digest))
}

impl Drop for HandleFormulaBenchmark {
    fn drop(&mut self) {
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
            .map(|worker| benchmark_topic_key("BENCH.DISTINCT", worker as u64))
            .collect::<Vec<_>>();

        for key in &keys {
            let factory_calls = Arc::clone(&factory_calls);
            runtime
                .prepare_observed(
                    *key,
                    move || {
                        factory_calls.fetch_add(1, Ordering::Relaxed);
                        Ok(Arc::new(BenchHandleObject { _payload: 0 }))
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
                                    Ok(Arc::new(BenchHandleObject { _payload: 0 }))
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

struct ContendedHandleBatch {
    key: HandleTopicKey,
}

/// A persistent worker pool for measuring same-key contention without thread
/// creation in the timed section.
pub struct HandleContendedBenchmark {
    runtime: Arc<HandleRuntime>,
    next_key: AtomicUsize,
    start_tx: Vec<std::sync::mpsc::SyncSender<ContendedHandleBatch>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HandleContendedBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(HANDLE_CONTENTION_CAPACITY, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(HANDLE_CONTENTION_WORKERS);
        let mut start_tx = Vec::with_capacity(HANDLE_CONTENTION_WORKERS);
        let mut workers = Vec::with_capacity(HANDLE_CONTENTION_WORKERS);

        for _ in 0..HANDLE_CONTENTION_WORKERS {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<ContendedHandleBatch>(1);
            let done_tx = done_tx.clone();
            let worker_runtime = Arc::clone(&runtime);
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while let Ok(batch) = worker_rx.recv() {
                    let result = worker_runtime
                        .prepare_observed(
                            batch.key,
                            || Ok(Arc::new(BenchHandleObject { _payload: 0 })),
                            |_, _| Ok(()),
                        )
                        .expect("contended handle publication failed");
                    std::hint::black_box(result);
                    done_tx
                        .send(())
                        .expect("benchmark driver received completion signal");
                }
            }));
        }

        Self {
            runtime,
            next_key: AtomicUsize::new(0),
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run(&self) {
        let id = self.next_key.fetch_add(1, Ordering::Relaxed);
        let key = benchmark_topic_key("BENCH.CONTENDED", id as u64);
        for start in &self.start_tx {
            start
                .send(ContendedHandleBatch { key })
                .expect("benchmark worker received start signal");
        }
        for _ in 0..self.start_tx.len() {
            self.done_rx
                .recv()
                .expect("benchmark worker finished batch processing");
        }
    }
}

impl Default for HandleContendedBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HandleContendedBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A persistent worker pool for measuring key allocation / string creation overhead.
pub struct HandleKeyAllocationBenchmark {
    iterations_per_worker: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HandleKeyAllocationBenchmark {
    pub fn new(worker_count: usize, iterations_per_worker: usize) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let keys = (0..worker_count)
            .map(|worker| Arc::<str>::from(format!("warm:{worker}")))
            .collect::<Vec<_>>();

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for key in keys {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while worker_rx.recv().is_ok() {
                    for _ in 0..iterations_per_worker {
                        let owned = key.to_string();
                        std::hint::black_box(owned);
                    }
                    done_tx
                        .send(())
                        .expect("benchmark driver received completion signal");
                }
            }));
        }

        Self {
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
                .expect("key-allocation benchmark worker received start signal");
        }
        for _ in 0..self.start_tx.len() {
            self.done_rx
                .recv()
                .expect("key-allocation benchmark worker finished batch");
        }
    }

    pub fn total_iterations(&self) -> usize {
        self.start_tx.len() * self.iterations_per_worker
    }
}

impl Drop for HandleKeyAllocationBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A persistent worker pool for measuring prepare and lease guard contention.
pub struct HandleGuardContentionBenchmark {
    runtime: Arc<HandleRuntime>,
    iterations_per_worker: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl HandleGuardContentionBenchmark {
    pub fn new(worker_count: usize, iterations_per_worker: usize) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let runtime = Arc::new(
            HandleRuntime::try_new_with_ingress(worker_count, None)
                .expect("benchmark host provides an OS CSPRNG"),
        );

        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            let worker_runtime = Arc::clone(&runtime);
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while worker_rx.recv().is_ok() {
                    for _ in 0..iterations_per_worker {
                        let prepare = worker_runtime.prepares.enter();
                        let lease = worker_runtime.leases.acquire();
                        std::hint::black_box((&prepare, &lease));
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
            start_tx,
            done_rx,
            workers,
        }
    }

    pub fn run(&self) {
        for start in &self.start_tx {
            start
                .send(())
                .expect("guard-contention benchmark worker received start signal");
        }
        for _ in 0..self.start_tx.len() {
            self.done_rx
                .recv()
                .expect("guard-contention benchmark worker finished batch");
        }
    }

    pub fn total_iterations(&self) -> usize {
        self.start_tx.len() * self.iterations_per_worker
    }
}

impl Drop for HandleGuardContentionBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        cleanup_handle_runtime(&self.runtime);
    }
}

/// Benchmark object for measuring structured formula topic key creation cost.
pub struct CreateFormulaTopicKeyBenchmark {
    caller: FormulaCaller,
    digest: [u8; 32],
}

impl CreateFormulaTopicKeyBenchmark {
    pub fn new() -> Self {
        Self {
            caller: FormulaCaller {
                sheet_id: 7,
                row: 42,
                column: 11,
            },
            digest: [42u8; 32],
        }
    }

    pub fn run(&self) {
        std::hint::black_box(FormulaTopicKey::new(
            self.caller,
            HANDLE_FORMULA_UDF_ID,
            &self.digest,
        ));
    }
}

impl Default for CreateFormulaTopicKeyBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

/// Benchmark object for measuring formula RTD key serialization cost.
pub struct FormatFormulaTopicKeyBenchmark {
    key: FormulaTopicKey,
}

impl FormatFormulaTopicKeyBenchmark {
    pub fn new() -> Self {
        Self {
            key: FormulaTopicKey::new(
                FormulaCaller {
                    sheet_id: 7,
                    row: 42,
                    column: 11,
                },
                HANDLE_FORMULA_UDF_ID,
                &[42u8; 32],
            ),
        }
    }

    pub fn run(&self) -> String {
        self.key.format_rtd_key()
    }
}

impl Default for FormatFormulaTopicKeyBenchmark {
    fn default() -> Self {
        Self::new()
    }
}
