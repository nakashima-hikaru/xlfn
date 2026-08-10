//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]

#[cfg(feature = "async")]
use crate::async_udf::AsyncManager;
#[cfg(feature = "async")]
use crate::cancellation::CancellationSource;
#[cfg(feature = "async")]
use crate::{CancellationGuarantee, XllError};
use std::sync::Arc;
#[cfg(feature = "async")]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "async")]
pub struct AsyncSpawnBenchmark {
    manager: Arc<AsyncManager>,
    generation: u64,
}

#[cfg(feature = "async")]
pub struct SpawnBatchResult {
    pub accepted: usize,
    pub overloaded: usize,
    pub other_errors: usize,
}

#[cfg(feature = "async")]
impl AsyncSpawnBenchmark {
    pub fn new(worker_count: usize) -> Self {
        let manager = Arc::new(AsyncManager::new());
        manager
            .start(worker_count)
            .expect("AsyncManager failed to start for benchmark");
        let generation = manager.current_generation();
        Self {
            manager,
            generation,
        }
    }

    pub fn run(&self, threads: usize, iterations_per_thread: usize) -> SpawnBatchResult {
        let accepted = Arc::new(AtomicUsize::new(0));
        let overloaded = Arc::new(AtomicUsize::new(0));
        let other_errors = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let mgr = Arc::clone(&self.manager);
                let current_gen = self.generation;
                let accepted = Arc::clone(&accepted);
                let overloaded = Arc::clone(&overloaded);
                let other_errors = Arc::clone(&other_errors);

                std::thread::spawn(move || {
                    for _ in 0..iterations_per_thread {
                        let (source, _token) =
                            CancellationSource::new(CancellationGuarantee::BestEffort);
                        match mgr.spawn(current_gen, async {}, source) {
                            Ok(()) => {
                                accepted.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(XllError::Overloaded) => {
                                overloaded.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(_) => {
                                other_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("benchmark thread panicked");
        }

        SpawnBatchResult {
            accepted: accepted.load(Ordering::SeqCst),
            overloaded: overloaded.load(Ordering::SeqCst),
            other_errors: other_errors.load(Ordering::SeqCst),
        }
    }
}

#[cfg(feature = "async")]
impl Drop for AsyncSpawnBenchmark {
    fn drop(&mut self) {
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
// Handle prepare benchmarks
// ---------------------------------------------------------------------------

use crate::handle::{ExcelHandleObject, HandleRuntime};

struct BenchHandleObject {
    _payload: u64,
}
impl ExcelHandleObject for BenchHandleObject {}

#[derive(Clone, Copy, Debug)]
pub enum HandlePrepareKind {
    /// First call for a key — factory is invoked.
    ColdMiss,
    /// Repeated call for an already-published key — factory is skipped.
    WarmHit,
    /// Multiple threads contend on the same key.
    Contended,
}

pub struct HandlePrepareBenchmark {
    runtime: Arc<HandleRuntime>,
}

impl Default for HandlePrepareBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl HandlePrepareBenchmark {
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(
                HandleRuntime::try_new_with_ingress(16_384, None)
                    .expect("benchmark host provides an OS CSPRNG"),
            ),
        }
    }

    pub fn run(&self, kind: HandlePrepareKind, iterations: usize) {
        match kind {
            HandlePrepareKind::ColdMiss => {
                for i in 0..iterations {
                    let key = format!("cold:{i}");
                    let _ = std::hint::black_box(
                        self.runtime
                            .prepare_observed(
                                key,
                                || Ok(Arc::new(BenchHandleObject { _payload: i as u64 })),
                                |_, _| Ok(()),
                            )
                            .unwrap(),
                    );
                }
            }
            HandlePrepareKind::WarmHit => {
                // Seed one topic, then repeatedly hit it.
                let key = "warm:0".to_owned();
                self.runtime
                    .prepare_observed(
                        key.clone(),
                        || Ok(Arc::new(BenchHandleObject { _payload: 0 })),
                        |_, _| Ok(()),
                    )
                    .unwrap();
                for _ in 0..iterations {
                    let _ = std::hint::black_box(
                        self.runtime
                            .prepare_observed(
                                key.clone(),
                                || Ok(Arc::new(BenchHandleObject { _payload: 1 })),
                                |_, _| Ok(()),
                            )
                            .unwrap(),
                    );
                }
            }
            HandlePrepareKind::Contended => {
                use std::sync::Barrier;

                let barrier = Arc::new(Barrier::new(4));
                let threads: Vec<_> = (0..4)
                    .map(|_| {
                        let b = Arc::clone(&barrier);
                        let rt = Arc::clone(&self.runtime);
                        std::thread::spawn(move || {
                            b.wait();
                            for i in 0..iterations {
                                let key = format!("contended:{i}");
                                let _ = std::hint::black_box(rt.prepare_observed(
                                    key,
                                    || Ok(Arc::new(BenchHandleObject { _payload: i as u64 })),
                                    |_, _| Ok(()),
                                ));
                            }
                        })
                    })
                    .collect();
                for t in threads {
                    t.join().expect("benchmark thread panicked");
                }
            }
        }
    }
}

impl Drop for HandlePrepareBenchmark {
    fn drop(&mut self) {
        self.runtime.terminate_all_topics();
        let _ = self.runtime.close();
    }
}
