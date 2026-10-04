//! Persistent-worker measurements of the production cancellation slot pool.

use super::CancellationRegistry;
use std::sync::{Arc, Barrier, mpsc};

/// Slot allocation and release, excluding thread creation and pool warmup.
pub struct CancellationLifecycleBenchmark {
    starts: Vec<mpsc::SyncSender<usize>>,
    done: mpsc::Receiver<usize>,
    start: Arc<Barrier>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl CancellationLifecycleBenchmark {
    /// Creates a cancellation fixture with the requested worker count and dispatch mode.
    pub fn new(workers: usize, dispatch_only: bool) -> Self {
        assert!(workers != 0);
        let registry = Arc::new(CancellationRegistry::new());
        let start = Arc::new(Barrier::new(workers + 1));
        let (done_tx, done) = mpsc::sync_channel(workers);
        let mut starts = Vec::with_capacity(workers);
        let mut threads = Vec::with_capacity(workers);
        for _ in 0..workers {
            let registry = Arc::clone(&registry);
            let start = Arc::clone(&start);
            let done = done_tx.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            starts.push(sender);
            threads.push(std::thread::spawn(move || {
                // Select and prime this worker's actual production shard.
                // Registry allocations are retained through all worker joins.
                for _ in 0..128 {
                    let (_, generation, slot) = registry.allocate();
                    registry.release(slot, generation);
                }
                done.send(0).expect("benchmark driver receives warmup");
                while let Ok(iterations) = receiver.recv() {
                    start.wait();
                    if !dispatch_only {
                        for _ in 0..iterations {
                            let (_, generation, slot) = registry.allocate();
                            registry.release(slot, generation);
                        }
                    }
                    done.send(iterations)
                        .expect("benchmark driver receives result");
                }
            }));
        }
        for _ in 0..workers {
            assert_eq!(done.recv().expect("benchmark worker warmed"), 0);
        }
        Self {
            starts,
            done,
            start,
            workers: threads,
        }
    }

    /// Performs cancellation iterations and returns observed completions.
    pub fn run(&self, iterations: usize) -> usize {
        for worker in &self.starts {
            worker
                .send(iterations)
                .expect("benchmark worker receives batch");
        }
        self.start.wait();
        (0..self.starts.len())
            .map(|_| self.done.recv().expect("benchmark worker finished batch"))
            .sum()
    }
}

impl Drop for CancellationLifecycleBenchmark {
    fn drop(&mut self) {
        self.starts.clear();
        for worker in self.workers.drain(..) {
            crate::panic_boundary::contain_panic(worker.join()).expect("benchmark worker panicked");
        }
    }
}
