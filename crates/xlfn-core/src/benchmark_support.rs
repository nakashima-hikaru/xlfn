//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]

use crate::async_udf::AsyncManager;
use crate::cancellation::CancellationSource;
use crate::{CancellationGuarantee, XllError};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct AsyncSpawnBenchmark {
    manager: Arc<AsyncManager>,
    generation: u64,
}

pub struct SpawnBatchResult {
    pub accepted: usize,
    pub overloaded: usize,
    pub other_errors: usize,
}

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

impl Drop for AsyncSpawnBenchmark {
    fn drop(&mut self) {
        let _ = self.manager.close();
    }
}
