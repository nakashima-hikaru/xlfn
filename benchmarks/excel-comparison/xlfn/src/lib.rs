//! Paired Excel workload functions. Change both implementations together.
// MSVC reports LNK4104 for the framework's intentional COM class exports.
#![allow(linker_messages)]

mod rtd_source;
use rtd_source::{BenchRtdSource, RtdShared};

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

mod async_control;
use async_control::AsyncControl;
use xlfn::{
    AsyncConfig, AsyncWorkerCount, RtdConfig, RuntimeConfig,
    error::{DomainErrorCode, InputError},
    prelude::*,
    rtd::{RtdCapacity, RtdLimits, RtdSourceHandle, RtdValue},
    value::ExcelCellValue,
};

struct CountingAllocator;
static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOC_BYTES.fetch_add(size as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

pub struct State {
    shared_read: AtomicU64,
    contended: AtomicU64,
    async_control: AsyncControl,
    rtd: RtdSourceHandle<BenchRtdSource>,
    rtd_shared: Arc<RtdShared>,
}

#[excel_addin(
    name = "xlfn Excel Comparison",
    id = "xlfn-excel-comparison",
    category = "Bench"
)]
pub struct BenchAddin;

impl Addin for BenchAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> XllResult<Opened<State>> {
        let (source, rtd_shared) = BenchRtdSource::new();
        let rtd = context.rtd().register_source(source)?;
        let limits = RtdLimits::standard()
            .with_max_pending(RtdCapacity::disabled_if_zero(120_000))
            .with_max_active(RtdCapacity::disabled_if_zero(120_000))
            .with_max_queued_updates(RtdCapacity::disabled_if_zero(120_000));
        let config =
            RuntimeConfig::new()
                .with_rtd(RtdConfig::new().with_limits(limits))
                .with_async(AsyncConfig::new().with_worker_count(
                    AsyncWorkerCount::new(32).expect("32 workers are supported"),
                ));
        Ok(Opened::new(State {
            shared_read: AtomicU64::new(0),
            contended: AtomicU64::new(0),
            async_control: AsyncControl::default(),
            rtd,
            rtd_shared,
        })
        .with_runtime_config(config))
    }
}

#[excel_function(name = "BENCH.ID", thread_safe)]
pub fn identity(value: f64) -> f64 {
    value
}

#[excel_function(name = "BENCH.SUM2", thread_safe)]
pub fn sum2(a: f64, b: f64) -> f64 {
    a + b
}

#[excel_function(name = "BENCH.SUM4", thread_safe)]
pub fn sum4(a: f64, b: f64, c: f64, d: f64) -> f64 {
    a + b + c + d
}

#[excel_function(name = "BENCH.SUM8", thread_safe)]
pub fn sum8(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64) -> f64 {
    a + b + c + d + e + f + g + h
}

fn busy(micros: i32) {
    if micros <= 0 {
        return;
    }
    let end = Instant::now() + Duration::from_micros(micros as u64);
    while Instant::now() < end {
        std::hint::spin_loop();
    }
}

#[excel_function(name = "BENCH.CPU", thread_safe)]
pub fn cpu(value: f64, micros: i32) -> f64 {
    busy(micros);
    value
}

#[excel_function(name = "BENCH.ERRNUM", thread_safe)]
pub fn error(value: f64, period: i32) -> XllResult<f64> {
    if period > 0 && (value as i64) % period as i64 == 0 {
        Err(XllError::Domain {
            code: DomainErrorCode::InvalidInput,
        })
    } else {
        Ok(value)
    }
}

#[excel_function(name = "BENCH.MAT.SUM", thread_safe)]
pub fn matrix_sum(values: Matrix<f64>) -> f64 {
    values.iter().copied().sum()
}

#[excel_function(name = "BENCH.MAT.MAKE", thread_safe)]
pub fn matrix_make(rows: i32, columns: i32, seed: f64) -> XllResult<Matrix<f64>> {
    let rows =
        usize::try_from(rows).map_err(|_| XllError::input("rows", InputError::OutOfRange))?;
    let columns =
        usize::try_from(columns).map_err(|_| XllError::input("columns", InputError::OutOfRange))?;
    let count = rows
        .checked_mul(columns)
        .ok_or_else(|| XllError::input("rows", InputError::OutOfRange))?;
    Matrix::new(rows, columns, (0..count).map(|i| seed + i as f64).collect())
}

#[excel_function(name = "BENCH.MAT.COPY", thread_safe)]
pub fn matrix_copy(values: Matrix<f64>) -> Matrix<f64> {
    values
}

#[excel_function(name = "BENCH.MAT.MIXED", thread_safe)]
pub fn matrix_mixed(values: Matrix<ExcelCellValue>) -> f64 {
    values
        .iter()
        .map(|cell| match cell {
            ExcelCellValue::Number(_) => 1.0,
            ExcelCellValue::String(_) => 2.0,
            ExcelCellValue::Boolean(_) => 3.0,
            ExcelCellValue::Blank => 4.0,
            ExcelCellValue::Error(_) => 5.0,
        })
        .sum()
}

#[excel_function(name = "BENCH.STR.IN", thread_safe)]
pub fn string_input(value: String) -> f64 {
    value.chars().count() as f64
}

#[excel_function(name = "BENCH.STR.OUT", thread_safe)]
pub fn string_output(length: i32, japanese: bool) -> String {
    let unit = if japanese { "日" } else { "a" };
    unit.repeat(length.max(0) as usize)
}

#[excel_function(name = "BENCH.STR.COPY", thread_safe)]
pub fn string_copy(value: String) -> String {
    value
}

#[excel_function(name = "BENCH.SHARED")]
pub fn shared_read(context: ThreadSafeContext<'_, BenchAddin>, value: f64) -> f64 {
    value + context.state().shared_read.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.CONTENDED")]
pub fn contended(context: ThreadSafeContext<'_, BenchAddin>, value: f64) -> f64 {
    value + context.state().contended.fetch_add(1, Ordering::AcqRel) as f64
}

#[excel_function(name = "BENCH.ALLOC.BYTES", thread_safe)]
pub fn alloc_bytes() -> f64 {
    ALLOC_BYTES.load(Ordering::Relaxed) as f64
}

/// Enables allocation counting without resetting the cumulative counter.
#[excel_function(name = "BENCH.ALLOC.TRACK")]
pub fn alloc_track(enabled: bool) -> f64 {
    TRACK_ALLOCATIONS.store(enabled, Ordering::Relaxed);
    if enabled { 1.0 } else { 0.0 }
}

#[excel_function(name = "BENCH.ASYNC")]
pub async fn async_value(
    context: AsyncContext<'_, BenchAddin>,
    value: f64,
    delay_us: i32,
) -> XllResult<f64> {
    let mut guard = context.state().async_control.enter(context.cancellation());
    context.check_cancelled()?;
    if delay_us < 0 {
        if let Some(receiver) = context.state().async_control.wait() {
            let _ = receiver.await;
        }
    } else if delay_us > 0 && delay_us < 1_000 {
        busy(delay_us);
    } else if delay_us > 0 {
        futures_timer::Delay::new(Duration::from_micros(delay_us as u64)).await;
    }
    context.check_cancelled()?;
    guard.complete();
    Ok(value)
}

#[excel_function(name = "BENCH.ASYNC.ARM")]
pub fn async_arm(
    context: MainThreadContext<'_, BenchAddin>,
    directory: String,
    expected: i32,
) -> XllResult<f64> {
    context
        .state()
        .async_control
        .arm(std::path::Path::new(&directory), expected as u64)
        .map_err(|error| XllError::Native {
            code: error.raw_os_error().unwrap_or(0),
            message: format!("cannot arm async gate: {error}"),
        })?;
    Ok(1.0)
}

#[excel_function(name = "BENCH.ASYNC.RELEASE")]
pub fn async_release(context: MainThreadContext<'_, BenchAddin>, sequence: f64) -> f64 {
    context.state().async_control.release();
    sequence
}

#[excel_function(name = "BENCH.ASYNC.ACTIVE")]
pub fn async_active(context: ThreadSafeContext<'_, BenchAddin>) -> f64 {
    context.state().async_control.active() as f64
}

#[excel_function(name = "BENCH.ASYNC.FINISHED")]
pub fn async_finished(context: ThreadSafeContext<'_, BenchAddin>) -> f64 {
    context.state().async_control.finished() as f64
}

#[excel_function(name = "BENCH.RTD")]
pub fn rtd_value(
    context: MainThreadContext<'_, BenchAddin>,
    topic: String,
    period_ms: f64,
) -> XllResult<RtdValue> {
    let period = period_ms.to_string();
    context
        .rtd()
        .subscribe(&context.state().rtd, &[topic.as_str(), period.as_str()])
}

#[excel_function(name = "BENCH.RTD.EMITTED")]
pub fn rtd_emitted(context: ThreadSafeContext<'_, BenchAddin>) -> f64 {
    context.state().rtd_shared.emitted.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.RTD.PULSE")]
pub fn rtd_pulse(context: MainThreadContext<'_, BenchAddin>, sequence: f64) -> f64 {
    context.state().rtd_shared.set_pulse(sequence as u64);
    sequence
}

#[excel_function(name = "BENCH.RTD.COUNT")]
pub fn rtd_count(context: ThreadSafeContext<'_, BenchAddin>) -> f64 {
    context.state().rtd_shared.count() as f64
}

include!(concat!(env!("OUT_DIR"), "/extra.rs"));
