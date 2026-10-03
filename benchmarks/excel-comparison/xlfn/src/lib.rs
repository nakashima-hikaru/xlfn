//! Paired Excel workload functions. Change both implementations together.
// MSVC reports LNK4104 for the framework's intentional COM class exports.
#![allow(linker_messages)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::HashMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use futures_channel::oneshot;
use xlfn::{
    AsyncConfig, AsyncWorkerCount, RtdConfig, RuntimeConfig,
    error::{DomainErrorCode, InputError},
    prelude::*,
    rtd::{
        RtdCapacity, RtdLimits, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription, RtdTopic,
        RtdValue,
    },
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
    async_active: AtomicU64,
    async_finished: AtomicU64,
    async_release: AtomicBool,
    async_waiters: Mutex<Vec<oneshot::Sender<()>>>,
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
            .with_max_pending(RtdCapacity::from_usize(120_000))
            .with_max_active(RtdCapacity::from_usize(120_000))
            .with_max_queued_updates(RtdCapacity::from_usize(120_000));
        let config =
            RuntimeConfig::new()
                .with_rtd(RtdConfig::new().with_limits(limits))
                .with_async(AsyncConfig::new().with_worker_count(
                    AsyncWorkerCount::new(32).expect("32 workers are supported"),
                ));
        Ok(Opened::new(State {
            shared_read: AtomicU64::new(0),
            contended: AtomicU64::new(0),
            async_active: AtomicU64::new(0),
            async_finished: AtomicU64::new(0),
            async_release: AtomicBool::new(false),
            async_waiters: Mutex::new(Vec::new()),
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
pub fn shared_read(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>,
    value: f64,
) -> f64 {
    value + context.state().shared_read.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.CONTENDED")]
pub fn contended(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>,
    value: f64,
) -> f64 {
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

struct ActiveGuard<'a>(&'a State);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.async_active.fetch_sub(1, Ordering::Relaxed);
        self.0.async_finished.fetch_add(1, Ordering::Relaxed);
    }
}

#[excel_function(name = "BENCH.ASYNC")]
pub async fn async_value(
    #[excel_context(asynchronous)] context: AsyncContext<'_, BenchAddin>,
    value: f64,
    delay_us: i32,
) -> XllResult<f64> {
    context.state().async_active.fetch_add(1, Ordering::Relaxed);
    let _guard = ActiveGuard(context.state());
    context.check_cancelled()?;
    if delay_us < 0 {
        let receiver = {
            let mut waiters = context
                .state()
                .async_waiters
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if context.state().async_release.load(Ordering::Acquire) {
                None
            } else {
                let (sender, receiver) = oneshot::channel();
                waiters.push(sender);
                Some(receiver)
            }
        };
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    } else if delay_us > 0 && delay_us < 1_000 {
        busy(delay_us);
    } else if delay_us > 0 {
        futures_timer::Delay::new(Duration::from_micros(delay_us as u64)).await;
    }
    context.check_cancelled()?;
    Ok(value)
}

#[excel_function(name = "BENCH.ASYNC.RELEASE")]
pub fn async_release(
    #[excel_context(main_thread)] context: MainThreadContext<'_, BenchAddin>,
    sequence: f64,
) -> f64 {
    context.state().async_release.store(true, Ordering::Release);
    let waiters = std::mem::take(
        &mut *context
            .state()
            .async_waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
    );
    for waiter in waiters {
        let _ = waiter.send(());
    }
    sequence
}

#[excel_function(name = "BENCH.ASYNC.ACTIVE")]
pub fn async_active(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>,
) -> f64 {
    context.state().async_active.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.ASYNC.FINISHED")]
pub fn async_finished(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>,
) -> f64 {
    context.state().async_finished.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.RTD")]
pub fn rtd_value(
    #[excel_context(main_thread)] context: MainThreadContext<'_, BenchAddin>,
    topic: String,
    period_ms: f64,
) -> XllResult<RtdValue> {
    let period = period_ms.to_string();
    context
        .rtd()
        .subscribe(&context.state().rtd, &[topic.as_str(), period.as_str()])
}

#[excel_function(name = "BENCH.RTD.EMITTED")]
pub fn rtd_emitted(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>,
) -> f64 {
    context.state().rtd_shared.emitted.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.RTD.PULSE")]
pub fn rtd_pulse(
    #[excel_context(main_thread)] context: MainThreadContext<'_, BenchAddin>,
    sequence: f64,
) -> f64 {
    context.state().rtd_shared.set_pulse(sequence as u64);
    sequence
}

#[excel_function(name = "BENCH.RTD.COUNT")]
pub fn rtd_count(#[excel_context(thread_safe)] context: ThreadSafeContext<'_, BenchAddin>) -> f64 {
    context
        .state()
        .rtd_shared
        .entries
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .len() as f64
}

struct RtdEntry {
    sink: RtdSink<RtdValue>,
    cancelled: Arc<AtomicBool>,
    interval: Option<Duration>,
    next_due: Instant,
    sequence: u64,
    last_pulse: u64,
}

struct RtdShared {
    entries: Mutex<HashMap<u64, RtdEntry>>,
    wake: Condvar,
    next_id: AtomicU64,
    pulse: AtomicU64,
    periodic_count: AtomicU64,
    fast_count: AtomicU64,
    emitted: AtomicU64,
    stop: AtomicBool,
}

impl RtdShared {
    fn set_pulse(&self, sequence: u64) {
        // The worker checks its wait predicate while holding entries. Taking
        // the same mutex keeps a pulse from being lost between check and wait.
        let _entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        self.pulse.store(sequence, Ordering::Release);
        self.wake.notify_one();
    }
}

struct BenchRtdSource {
    shared: Arc<RtdShared>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl BenchRtdSource {
    fn new() -> (Self, Arc<RtdShared>) {
        let shared = Arc::new(RtdShared {
            entries: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            next_id: AtomicU64::new(1),
            pulse: AtomicU64::new(0),
            periodic_count: AtomicU64::new(0),
            fast_count: AtomicU64::new(0),
            emitted: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        (
            Self {
                shared: shared.clone(),
                worker: Mutex::new(None),
            },
            shared,
        )
    }

    fn ensure_worker(&self) {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_some() {
            return;
        }
        let worker_shared = self.shared.clone();
        *worker = Some(thread::spawn(move || {
            let mut handled_pulse = 0;
            let mut entries = worker_shared
                .entries
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            loop {
                while !worker_shared.stop.load(Ordering::Acquire)
                    && worker_shared.pulse.load(Ordering::Acquire) == handled_pulse
                    && worker_shared.periodic_count.load(Ordering::Acquire) == 0
                {
                    entries = worker_shared
                        .wake
                        .wait(entries)
                        .unwrap_or_else(|p| p.into_inner());
                }
                if worker_shared.stop.load(Ordering::Acquire) {
                    break;
                }
                let pulse = worker_shared.pulse.load(Ordering::Acquire);
                let now = Instant::now();
                for entry in entries.values_mut() {
                    if entry.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    if pulse > entry.last_pulse {
                        entry.last_pulse = pulse;
                        let _ = entry.sink.publish(RtdValue::Number(pulse as f64));
                        worker_shared.emitted.fetch_add(1, Ordering::Relaxed);
                    } else if let Some(interval) = entry.interval
                        && now >= entry.next_due
                    {
                        entry.sequence += 1;
                        let _ = entry.sink.publish(RtdValue::Number(entry.sequence as f64));
                        worker_shared.emitted.fetch_add(1, Ordering::Relaxed);
                        entry.next_due = now + interval;
                    }
                }
                handled_pulse = pulse;
                if worker_shared.fast_count.load(Ordering::Acquire) > 0 {
                    drop(entries);
                    std::hint::spin_loop();
                    entries = worker_shared
                        .entries
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                } else if worker_shared.periodic_count.load(Ordering::Acquire) > 0 {
                    entries = worker_shared
                        .wake
                        .wait_timeout(entries, Duration::from_millis(1))
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                }
            }
        }));
    }
}

impl Drop for BenchRtdSource {
    fn drop(&mut self) {
        {
            let _entries = self
                .shared
                .entries
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            self.shared.stop.store(true, Ordering::Release);
            self.shared.wake.notify_one();
        }
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

struct BenchSubscription {
    shared: Arc<RtdShared>,
    id: u64,
    cancelled: Arc<AtomicBool>,
}

// SAFETY: the worker publishes only while holding entries. Removal takes that
// mutex, then drops the sole stored sink before disconnect_and_wait returns.
unsafe impl RtdSubscription for BenchSubscription {
    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
        self.cancelled.store(true, Ordering::Release);
        let mut entries = self
            .shared
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let removed = entries.remove(&self.id);
        if removed
            .as_ref()
            .is_some_and(|entry| entry.interval.is_some())
        {
            self.shared.periodic_count.fetch_sub(1, Ordering::AcqRel);
        }
        if removed.as_ref().is_some_and(|entry| {
            entry
                .interval
                .is_some_and(|interval| interval < Duration::from_millis(1))
        }) {
            self.shared.fast_count.fetch_sub(1, Ordering::AcqRel);
        }
        drop(entries);
        self.shared.wake.notify_one();
        Ok(())
    }
}

// SAFETY: on success the returned subscription owns the only sink user. On
// error the supplied sink remains local; disconnect synchronizes with publish.
unsafe impl RtdSource for BenchRtdSource {
    type Value = RtdValue;
    type Subscription = BenchSubscription;

    fn subscribe(&self, topic: &RtdTopic, sink: RtdSink<RtdValue>) -> XllResult<BenchSubscription> {
        let period_ms: f64 = topic
            .part(1)
            .ok_or_else(|| XllError::input("period_ms", InputError::OutOfRange))?
            .parse()
            .map_err(|_| XllError::input("period_ms", InputError::OutOfRange))?;
        if !period_ms.is_finite() || period_ms < 0.0 {
            return Err(XllError::input("period_ms", InputError::OutOfRange));
        }
        let interval = (period_ms > 0.0).then(|| Duration::from_secs_f64(period_ms / 1000.0));
        let next_due = Instant::now() + interval.unwrap_or_default();
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        // Starting before sink transfer keeps a spawn failure from leaving a
        // sink in the shared map without a subscription to disconnect it.
        self.ensure_worker();
        let mut entries = self
            .shared
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if period_ms > 0.0 {
            self.shared.periodic_count.fetch_add(1, Ordering::AcqRel);
        }
        if period_ms > 0.0 && period_ms < 1.0 {
            self.shared.fast_count.fetch_add(1, Ordering::AcqRel);
        }
        entries.insert(
            id,
            RtdEntry {
                sink,
                cancelled: cancelled.clone(),
                interval,
                next_due,
                sequence: 0,
                last_pulse: self.shared.pulse.load(Ordering::Acquire),
            },
        );
        drop(entries);
        self.shared.wake.notify_one();
        Ok(BenchSubscription {
            shared: self.shared.clone(),
            id,
            cancelled,
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/extra.rs"));

#[cfg(test)]
mod rtd_source_tests {
    use super::BenchRtdSource;
    use std::sync::{Arc, atomic::Ordering, mpsc};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn rtd_source_has_no_worker_before_first_subscription() {
        let (source, shared) = BenchRtdSource::new();
        shared.set_pulse(7);
        assert!(source.worker.lock().unwrap().is_none());
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 0);
        drop(source);
        assert!(shared.stop.load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_start_reuses_worker_and_idle_shutdown_wakes_it() {
        let (source, shared) = BenchRtdSource::new();
        let source = Arc::new(source);
        let starters: Vec<_> = (0..4)
            .map(|_| {
                let source = source.clone();
                thread::spawn(move || {
                    source.ensure_worker();
                    source
                        .worker
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .thread()
                        .id()
                })
            })
            .collect();
        let ids: Vec<_> = starters
            .into_iter()
            .map(|starter| starter.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| *id == ids[0]));

        shared.set_pulse(1);
        let (finished, receiver) = mpsc::channel();
        let closer = thread::spawn(move || {
            drop(source);
            finished.send(()).unwrap();
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("source shutdown must wake and join its idle worker");
        closer.join().unwrap();
        assert!(shared.stop.load(Ordering::Acquire));
        assert_eq!(shared.emitted.load(Ordering::Relaxed), 0);
    }
}
