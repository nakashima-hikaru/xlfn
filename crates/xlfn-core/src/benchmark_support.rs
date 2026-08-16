//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]
#![allow(unsafe_code, reason = "Benchmark-only XLOPER12 pointer construction")]

use crate::ExcelParameter;
#[cfg(feature = "async")]
use crate::async_udf::AsyncManager;
#[cfg(feature = "async")]
use crate::cancellation::CancellationSource;
#[cfg(feature = "bench-input-identity-diagnostic")]
use crate::input_identity::{
    ARGUMENT_DOMAIN, InputIdentityEncoder, ROOT_DOMAIN, ROOT_PREFIX_BYTES,
};
#[cfg(feature = "async")]
use crate::{CancellationGuarantee, XllError};
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
    _runtime: Arc<crate::Runtime<()>>,
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
        let runtime = Arc::new(crate::Runtime::<()>::new());
        let close_epoch = runtime.close_epoch();
        let mut open_attempt = runtime
            .begin_open_if_epoch(close_epoch)
            .expect("begin_open");
        runtime.publish_state(());
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

            let r = Arc::clone(&runtime);
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
                                    &r,
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

#[cfg(feature = "bench-input-identity-diagnostic")]
use crate::Matrix;
use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaRevisionKey, HandleRuntime, HandleTopicKey,
    resolve_formula_caller,
};
use crate::host_callback::HostCallbackSession;
use crate::{InputFingerprint, OwnedExcelValue};

struct BenchHandleObject {
    _payload: u64,
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

// ---------------------------------------------------------------------------
// Formula-to-handle end-to-end benchmarks
// ---------------------------------------------------------------------------

const HANDLE_FORMULA_UDF_ID: &str = "BENCH.HANDLE";

#[derive(Clone, Copy, Debug)]
pub enum FormulaRevisionBenchCase {
    ScalarNumber,
    ShortString,
    Utf16String32KiB,
    NumericCells10K,
    NumericCells100K,
}

impl FormulaRevisionBenchCase {
    pub const ALL: [Self; 5] = [
        Self::ScalarNumber,
        Self::ShortString,
        Self::Utf16String32KiB,
        Self::NumericCells10K,
        Self::NumericCells100K,
    ];

    pub const END_TO_END: [Self; 3] = [
        Self::ScalarNumber,
        Self::ShortString,
        Self::NumericCells100K,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ScalarNumber => "scalar_number",
            Self::ShortString => "short_string",
            Self::Utf16String32KiB => "utf16_string_32kib",
            Self::NumericCells10K => "numeric_cells_10k",
            Self::NumericCells100K => "numeric_cells_100k",
        }
    }
}

/// Converted semantic arguments whose identity is measured before the timed
/// handle lookup.
struct PreparedFormulaArguments {
    value: OwnedExcelValue,
}

impl PreparedFormulaArguments {
    fn new(case: FormulaRevisionBenchCase) -> Self {
        let value = match case {
            FormulaRevisionBenchCase::ScalarNumber => OwnedExcelValue::Number(42.0),
            FormulaRevisionBenchCase::ShortString => OwnedExcelValue::String("short".to_owned()),
            FormulaRevisionBenchCase::Utf16String32KiB => OwnedExcelValue::String(
                (0..16 * 1024)
                    .map(|index| char::from(b'a' + (index % 26) as u8))
                    .collect(),
            ),
            FormulaRevisionBenchCase::NumericCells10K => Self::numeric_array(10_000),
            FormulaRevisionBenchCase::NumericCells100K => Self::numeric_array(100_000),
        };

        Self { value }
    }

    fn numeric_array(cells: usize) -> OwnedExcelValue {
        let columns = cells.min(10_000);
        let rows = cells.div_ceil(columns);
        OwnedExcelValue::Matrix(
            crate::Matrix::new(
                rows,
                columns,
                (0..cells)
                    .map(|index| OwnedExcelValue::Number(index as f64))
                    .collect(),
            )
            .expect("benchmark matrix dimensions must be valid"),
        )
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        let mut builder = crate::input_identity::InputFingerprintBuilder::new(1);
        builder
            .with_argument::<OwnedExcelValue, _, _>(|encoder| {
                self.value.encode_identity(encoder);
                Ok(())
            })
            .expect("benchmark semantic argument must fingerprint successfully");
        builder
            .finish()
            .expect("benchmark semantic argument fingerprint must finish")
            .as_bytes()
            .to_owned()
    }
}

pub struct InputIdentityBenchmark {
    arguments: PreparedFormulaArguments,
}

impl InputIdentityBenchmark {
    pub fn new(case: FormulaRevisionBenchCase) -> Self {
        Self {
            arguments: PreparedFormulaArguments::new(case),
        }
    }

    pub fn run(&self) -> [u8; 32] {
        self.arguments.fingerprint()
    }
}

#[cfg(feature = "bench-input-identity-diagnostic")]
pub struct InputIdentityDiagnosticBenchmark {
    owned_number: OwnedExcelValue,
    short_string: OwnedExcelValue,
    matrix_f64_100k: Matrix<f64>,
    f64_argument_digest: [u8; 32],
}

#[cfg(feature = "bench-input-identity-diagnostic")]
impl InputIdentityDiagnosticBenchmark {
    pub fn new() -> Self {
        let f64 = 42.0_f64;
        let matrix_f64_100k =
            Matrix::new(10_000, 10, (0..100_000).map(|index| index as f64).collect())
                .expect("diagnostic matrix dimensions must be valid");
        let benchmark = Self {
            owned_number: OwnedExcelValue::Number(f64),
            short_string: OwnedExcelValue::String("short".to_owned()),
            matrix_f64_100k,
            f64_argument_digest: Self::argument_digest::<0, _>(b"xlfn.input.f64.v4", |encoder| {
                encoder.f64(f64)
            }),
        };
        assert_eq!(
            benchmark.f64_full_current(),
            benchmark.f64_batched_64(),
            "identity batching changed the v4 f64 byte stream"
        );
        assert_eq!(
            benchmark.matrix_f64_100k_batched_64(),
            benchmark.matrix_f64_100k_batched_256(),
            "identity batching changed the v4 matrix byte stream"
        );
        assert_eq!(
            benchmark.matrix_f64_100k_batched_64(),
            benchmark.matrix_f64_100k_batched_1024(),
            "identity batching changed the v4 matrix byte stream"
        );
        benchmark
    }

    pub fn f64_full_current(&self) -> [u8; 32] {
        Self::fingerprint::<0, false, _>(b"xlfn.input.f64.v4", |encoder| encoder.f64(42.0))
    }

    pub fn f64_argument_only(&self) -> [u8; 32] {
        Self::argument_digest::<0, _>(b"xlfn.input.f64.v4", |encoder| encoder.f64(42.0))
    }

    pub fn f64_root_only(&self) -> [u8; 32] {
        Self::root_digest(&self.f64_argument_digest)
    }

    pub fn blake3_streaming_8b(&self) -> [u8; 32] {
        let bytes = [0_u8; 8];
        let mut hasher = blake3::Hasher::new();
        for byte in bytes {
            hasher.update(std::slice::from_ref(&byte));
        }
        *hasher.finalize().as_bytes()
    }

    pub fn blake3_one_shot_8b(&self) -> [u8; 32] {
        *blake3::hash(&[0_u8; 8]).as_bytes()
    }

    pub fn owned_number_full_current(&self) -> [u8; 32] {
        let value = match &self.owned_number {
            OwnedExcelValue::Number(value) => *value,
            _ => unreachable!("diagnostic fixture must be a number"),
        };
        Self::fingerprint::<0, false, _>(b"xlfn.input.owned-excel-value.v4", |encoder| {
            encoder.tag(0);
            encoder.f64(value);
        })
    }

    pub fn string_short_full_current(&self) -> [u8; 32] {
        let value = match &self.short_string {
            OwnedExcelValue::String(value) => value.as_str(),
            _ => unreachable!("diagnostic fixture must be a string"),
        };
        Self::fingerprint::<0, false, _>(b"xlfn.input.owned-excel-value.v4", |encoder| {
            encoder.tag(3);
            encoder.string(value);
        })
    }

    pub fn f64_batched_64(&self) -> [u8; 32] {
        Self::fingerprint::<64, true, _>(b"xlfn.input.f64.v4", |encoder| encoder.f64(42.0))
    }

    pub fn f64_batched_256(&self) -> [u8; 32] {
        Self::fingerprint::<256, true, _>(b"xlfn.input.f64.v4", |encoder| encoder.f64(42.0))
    }

    pub fn f64_batched_1024(&self) -> [u8; 32] {
        Self::fingerprint::<1024, true, _>(b"xlfn.input.f64.v4", |encoder| encoder.f64(42.0))
    }

    pub fn matrix_f64_100k_batched_64(&self) -> [u8; 32] {
        self.matrix_f64_100k_fingerprint::<64>()
    }

    pub fn matrix_f64_100k_batched_256(&self) -> [u8; 32] {
        self.matrix_f64_100k_fingerprint::<256>()
    }

    pub fn matrix_f64_100k_batched_1024(&self) -> [u8; 32] {
        self.matrix_f64_100k_fingerprint::<1024>()
    }

    fn matrix_f64_100k_fingerprint<const WRITE_BUFFER: usize>(&self) -> [u8; 32] {
        let matrix = &self.matrix_f64_100k;
        Self::fingerprint::<WRITE_BUFFER, true, _>(b"xlfn.input.matrix.v4", |encoder| {
            encoder.domain(b"xlfn.input.f64.v4");
            encoder.u64(matrix.rows() as u64);
            encoder.u64(matrix.columns() as u64);
            for value in matrix.as_slice() {
                encoder.f64(*value);
            }
        })
    }

    fn fingerprint<const WRITE_BUFFER: usize, const BATCH_ROOT: bool, F>(
        argument_domain: &[u8],
        encode: F,
    ) -> [u8; 32]
    where
        F: FnOnce(&mut InputIdentityEncoder<'_, WRITE_BUFFER>),
    {
        let argument_digest = Self::argument_digest::<WRITE_BUFFER, _>(argument_domain, encode);
        Self::root_digest_with_mode(&argument_digest, BATCH_ROOT)
    }

    fn argument_digest<const WRITE_BUFFER: usize, F>(argument_domain: &[u8], encode: F) -> [u8; 32]
    where
        F: FnOnce(&mut InputIdentityEncoder<'_, WRITE_BUFFER>),
    {
        let mut hasher = blake3::Hasher::new();
        let mut encoder = InputIdentityEncoder::<WRITE_BUFFER>::new(&mut hasher);
        encoder.domain(ARGUMENT_DOMAIN);
        encoder.domain(argument_domain);
        encode(&mut encoder);
        encoder
            .finish()
            .expect("diagnostic argument identity must finish")
    }

    fn root_digest(argument_digest: &[u8; 32]) -> [u8; 32] {
        Self::root_digest_with_mode(argument_digest, false)
    }

    fn root_digest_with_mode(argument_digest: &[u8; 32], batched: bool) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        if batched {
            let mut prefix = [0_u8; ROOT_PREFIX_BYTES];
            prefix[..8].copy_from_slice(&(ROOT_DOMAIN.len() as u64).to_le_bytes());
            prefix[8..8 + ROOT_DOMAIN.len()].copy_from_slice(ROOT_DOMAIN);
            prefix[8 + ROOT_DOMAIN.len()..].copy_from_slice(&1_u64.to_le_bytes());
            hasher.update(&prefix);
        } else {
            hasher.update(&(ROOT_DOMAIN.len() as u64).to_le_bytes());
            hasher.update(ROOT_DOMAIN);
            hasher.update(&1_u64.to_le_bytes());
        }
        hasher.update(argument_digest);
        *hasher.finalize().as_bytes()
    }
}

#[cfg(feature = "bench-input-identity-diagnostic")]
impl Default for InputIdentityDiagnosticBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FormulaRevisionBenchmark {
    runtime: Arc<HandleRuntime>,
    arguments: PreparedFormulaArguments,
    caller: FormulaCaller,
    factory_calls: AtomicUsize,
}

impl FormulaRevisionBenchmark {
    pub fn new(case: FormulaRevisionBenchCase) -> Self {
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
        let key = formula_revision_key(&arguments, caller);

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
            arguments,
            caller,
            factory_calls,
        }
    }

    pub fn run(&self) -> (String, bool) {
        let key = formula_revision_key(&self.arguments, self.caller);
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

fn formula_revision_key(
    arguments: &PreparedFormulaArguments,
    caller: FormulaCaller,
) -> HandleTopicKey {
    let inputs = arguments.fingerprint();
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        caller,
        HANDLE_FORMULA_UDF_ID,
        InputFingerprint::from_bytes(inputs),
    ))
}

impl Drop for FormulaRevisionBenchmark {
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
