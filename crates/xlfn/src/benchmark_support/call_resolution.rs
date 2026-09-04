use super::handle::benchmark_revision_key;
use super::*;

pub struct MultiHandleCallBenchmark {
    runtime: &'static crate::runtime::Runtime<()>,
    raw_tokens: Vec<xlfn_sys::XLOPER12>,
    _storage: Vec<Vec<u16>>,
}

impl MultiHandleCallBenchmark {
    pub fn new(count: usize) -> Self {
        assert!(count > 0, "benchmark handle count must be non-zero");
        let runtime = get_benchmark_runtime();
        let (raw_tokens, storage) = runtime
            .with_formula_handle_service(|handle_runtime| {
                let mut raw_tokens = Vec::with_capacity(count);
                let mut storage = Vec::with_capacity(count);

                for index in 0..count {
                    let key = benchmark_revision_key("BENCH.MULTI.HANDLE", index as u64);
                    let token = handle_runtime
                        .prepare_observed(
                            key,
                            move || {
                                Ok(BenchHandleObject {
                                    _payload: index as u64,
                                })
                            },
                            |_, _| Ok(()),
                        )
                        .expect("benchmark handle preparation must succeed")
                        .into_token();

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

                (raw_tokens, storage)
            })
            .expect("benchmark handle runtime must initialize");
        Self {
            runtime,
            raw_tokens,
            _storage: storage,
        }
    }

    pub fn run(&mut self) {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut frame = crate::__private::v1::CallFrame::<
                <f64 as crate::call_return::ExcelReturn>::InputMode,
            >::new(call, scope, 1);
            for raw in &mut self.raw_tokens {
                // SAFETY: raw points to valid benchmark storage.
                let handle: crate::handle::Handle<'_, BenchHandleObject> = unsafe {
                    frame
                        .convert_argument(0, "arg", raw)
                        .expect("benchmark argument conversion must succeed")
                };
                std::hint::black_box(handle);
            }
            let return_ctx = frame.return_context("bench_udf");
            let _ = std::hint::black_box(return_ctx);
        });
    }
}

pub struct ConcurrentHandleResolutionBenchmark {
    _runtime: &'static crate::runtime::Runtime<()>,
    threads: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl ConcurrentHandleResolutionBenchmark {
    pub fn new(threads: usize, iterations_per_thread: usize) -> Self {
        let runtime = get_benchmark_runtime();
        runtime
            .with_formula_handle_service(|_| ())
            .expect("benchmark handle runtime must initialize");

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
                        runtime
                            .with_generation_services(|services| {
                                let resolver = services.handle_call_access();
                                let rt = resolver.get().expect("handle runtime must resolve");
                                std::hint::black_box(rt);
                            })
                            .expect("benchmark generation remains published");
                    }
                    let _ = d_tx.send(());
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
