use super::handle::{benchmark_revision_key, cleanup_handle_runtime};
use super::*;

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
    runtime: Arc<FormulaHandleService>,
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
            FormulaHandleService::try_new(worker_count)
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let mut tokens = Vec::with_capacity(worker_count);
        for worker in 0..worker_count {
            let key = benchmark_revision_key("BENCH.LOOKUP", worker as u64);
            let token = runtime
                .prepare_observed(key, || Ok(BenchHandleObject { _payload: 0 }), |_, _| Ok(()))
                .expect("handle lookup warm seed publication failed")
                .into_token();
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
                        let result = crate::value::with_excel_call_scope(|scope| {
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

/// Control benchmark for the ownership cost of an `Arc<T>` payload.
///
/// This is intentionally not an alternative handle implementation: it omits
/// token lookup, publication, and epoch admission. It provides a lower-bound
/// ownership control beside [`HandleLookupBenchmark`] so the cost of the
/// current call-scoped EBR path can be evaluated against a direct strong-count
/// increment/decrement under the same worker topology.
pub struct ArcHandleLookupBenchmark {
    payload: Arc<BenchHandleObject>,
    worker_count: usize,
    iterations_per_worker: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl ArcHandleLookupBenchmark {
    pub fn new(worker_count: usize, iterations_per_worker: usize) -> Self {
        assert!(worker_count != 0);
        assert!(iterations_per_worker != 0);

        let payload = Arc::new(BenchHandleObject { _payload: 0 });
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(worker_count);
        let mut start_tx = Vec::with_capacity(worker_count);
        let mut workers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            let done_tx = done_tx.clone();
            let worker_payload = Arc::clone(&payload);
            start_tx.push(worker_tx);
            workers.push(std::thread::spawn(move || {
                while worker_rx.recv().is_ok() {
                    for _ in 0..iterations_per_worker {
                        let value = Arc::clone(&worker_payload);
                        std::hint::black_box(value);
                    }
                    done_tx
                        .send(())
                        .expect("Arc ownership benchmark driver received completion signal");
                }
            }));
        }

        Self {
            payload,
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
                .expect("Arc ownership benchmark worker received start signal");
        }
        for _ in 0..self.worker_count {
            self.done_rx
                .recv()
                .expect("Arc ownership benchmark worker finished batch");
        }
        std::hint::black_box(&self.payload);
    }

    pub fn total_iterations(&self) -> usize {
        self.worker_count * self.iterations_per_worker
    }
}

impl Drop for ArcHandleLookupBenchmark {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A persistent worker pool that looks up a different warm topic per worker.
///
/// The topics, worker threads, and synchronization channels are all prepared
/// before measurement. Each timed batch therefore exercises concurrent
/// `TopicState` access without paying for cold publication or thread creation.
pub struct HandleDistinctKeyBenchmark {
    runtime: Arc<FormulaHandleService>,
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
            FormulaHandleService::try_new(worker_count)
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
