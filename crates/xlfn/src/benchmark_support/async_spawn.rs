#[cfg(feature = "async")]
use super::*;
#[cfg(feature = "async")]
use std::future::Future;
#[cfg(feature = "async")]
use std::pin::Pin;
#[cfg(feature = "async")]
use std::task::{Context, Poll};

#[cfg(feature = "async")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncSpawnKind {
    Noop,
    Reschedule(usize),
}

#[cfg(feature = "async")]
pub struct RescheduleFuture {
    remaining: usize,
}

#[cfg(feature = "async")]
impl RescheduleFuture {
    pub const fn new(yields: usize) -> Self {
        Self { remaining: yields }
    }
}

#[cfg(feature = "async")]
impl Future for RescheduleFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.remaining == 0 {
            Poll::Ready(())
        } else {
            self.remaining -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[cfg(feature = "async")]
pub struct AsyncSpawnBenchmark {
    manager: Arc<AsyncManager>,
    start_tx: Vec<std::sync::mpsc::SyncSender<usize>>,
    done_rx: std::sync::mpsc::Receiver<SpawnBatchResult>,
    producers: Vec<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "async")]
#[derive(Default, Debug)]
pub struct SpawnBatchResult {
    pub accepted: usize,
    pub overloaded: usize,
    pub other_errors: usize,
}

#[cfg(feature = "async")]
impl AsyncSpawnBenchmark {
    pub fn new(worker_count: usize, producer_count: usize) -> Self {
        Self::new_with_kind(worker_count, producer_count, AsyncSpawnKind::Noop)
    }

    pub fn new_with_kind(worker_count: usize, producer_count: usize, kind: AsyncSpawnKind) -> Self {
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

                        let res = match kind {
                            AsyncSpawnKind::Noop => manager.spawn(generation, async {}, source),
                            AsyncSpawnKind::Reschedule(yields) => {
                                manager.spawn(generation, RescheduleFuture::new(yields), source)
                            }
                        };

                        match res {
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

    pub fn run_and_drain(&self, iterations_per_thread: usize) -> SpawnBatchResult {
        let result = self.run(iterations_per_thread);
        assert!(
            self.manager.wait_idle(),
            "executor suffered fatal worker failure during benchmark"
        );
        result
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
