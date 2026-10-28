#[derive(Clone, Copy, Debug)]
pub enum SyncBenchKind {
    IngressUdfOnly,
    FullAdmission,
    ScalarReturnNoSubscriber,
    ScalarReturnUdfTraceEnabled,
    WorksheetErrorNoSubscriber,
    DomainErrorNoSubscriber,
    ScalarArguments(usize),
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
    _runtime: &'static crate::runtime::Runtime<super::BenchmarkAddin>,
    threads: usize,
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl SyncBoundaryWorkerPool {
    pub fn new(threads: usize, iterations_per_thread: usize, kind: SyncBenchKind) -> Self {
        let ingress = crate::module_runtime::ingress();
        if ingress.phase() != crate::ingress::PHASE_CLOSED {
            ingress.begin_close_with(|| {});
            let _ = ingress.seal_and_drain();
        }
        let runtime: &'static crate::runtime::Runtime<super::BenchmarkAddin> = Box::leak(Box::new(
            crate::runtime::Runtime::<super::BenchmarkAddin>::new(),
        ));
        let removal_epoch = runtime.removal_epoch();
        let open_attempt = runtime
            .begin_open_if_epoch(removal_epoch)
            .expect("begin_open");
        let mut open_attempt = runtime.publish(open_attempt, (), ());
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

            let r = runtime;
            let handle = std::thread::spawn(move || {
                let arguments: [xlfn_sys::XLOPER12; 8] =
                    std::array::from_fn(|index| xlfn_sys::XLOPER12::number(index as f64 + 0.5));
                let _subscriber_guard = matches!(kind, SyncBenchKind::ScalarReturnUdfTraceEnabled)
                    .then(install_benchmark_subscriber);

                while s_rx.recv().is_ok() {
                    match kind {
                        SyncBenchKind::IngressUdfOnly => {
                            for _ in 0..iterations_per_thread {
                                let entry = crate::module_runtime::ingress().enter_udf_with(|| {});
                                std::hint::black_box(matches!(
                                    &entry,
                                    crate::ingress::ExportEntry::Admitted(_)
                                ));
                                drop(entry);
                            }
                        }
                        SyncBenchKind::FullAdmission => {
                            for _ in 0..iterations_per_thread {
                                let entry = crate::module_runtime::ingress().enter_udf_with(|| {});
                                if let Ok(ingress) = entry.into_admitted()
                                    && let Ok(call) = r.enter(&ingress)
                                {
                                    std::hint::black_box(&call);
                                }
                            }
                        }
                        SyncBenchKind::ScalarReturnNoSubscriber
                        | SyncBenchKind::ScalarReturnUdfTraceEnabled
                        | SyncBenchKind::WorksheetErrorNoSubscriber
                        | SyncBenchKind::DomainErrorNoSubscriber => {
                            for _ in 0..iterations_per_thread {
                                let ptr = crate::return_abi::udf_boundary_named(
                                    r,
                                    "bench_udf",
                                    "BENCH.UDF",
                                    |_, _| match kind {
                                        SyncBenchKind::WorksheetErrorNoSubscriber => Err(
                                            crate::XllError::ExcelValue(crate::ExcelError::Number),
                                        ),
                                        SyncBenchKind::DomainErrorNoSubscriber => {
                                            Err(crate::XllError::Domain {
                                                code: crate::error::DomainErrorCode::InvalidInput,
                                            })
                                        }
                                        _ => crate::return_abi::XlScalarOutput::encode(42.0),
                                    },
                                );
                                #[allow(
                                    unsafe_code,
                                    reason = "Internal benchmark resource cleanup"
                                )]
                                // SAFETY: ptr is the live root produced by this
                                // boundary. Excel calls xlAutoFree12 only for
                                // roots whose DLL-free bit is set.
                                let value = unsafe { &*ptr };
                                std::hint::black_box(value.xltype);
                                if value.xltype & xlfn_sys::XLBIT_DLL_FREE != 0 {
                                    // SAFETY: The live root requests one owning cleanup.
                                    let _ = unsafe { crate::return_abi::free_return_boundary(ptr) };
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
                        SyncBenchKind::ScalarArguments(argument_count) => {
                            assert!((1..=arguments.len()).contains(&argument_count));
                            for _ in 0..iterations_per_thread {
                                let pointer = crate::return_abi::udf_boundary_named(
                                    r,
                                    "bench_arguments",
                                    "BENCH.ARGUMENTS",
                                    |_, call| {
                                        let callbacks =
                                            crate::host_callback::HostCallbackSession::new();
                                        crate::call::with_excel_callback_scope_and_call(
                                            call,
                                            &callbacks,
                                            |call, scope, callbacks| {
                                                let mut frame =
                                                    crate::__private::v1::CallFrame::<
                                                        crate::value::input::PlainInputMode,
                                                    >::new(
                                                        call, scope, callbacks, argument_count
                                                    );
                                                let mut sum = 0.0;
                                                for (index, raw) in arguments
                                                    .iter()
                                                    .take(argument_count)
                                                    .enumerate()
                                                {
                                                    // SAFETY: The worker's immutable argument
                                                    // fixtures outlive this synchronous call.
                                                    let value = unsafe {
                                                        frame.prepare_argument::<f64>(
                                                            index,
                                                            "arg",
                                                            std::ptr::from_ref(raw).cast_mut(),
                                                        )
                                                    }?
                                                    .materialize()?;
                                                    sum += value;
                                                }
                                                let mut context =
                                                    frame.return_context("bench_arguments")?;
                                                crate::call_return::ExcelReturn::into_excel(
                                                    sum,
                                                    &mut context,
                                                )
                                            },
                                        )
                                    },
                                );
                                // SAFETY: Consume the live scalar before the next
                                // call, and follow Excel's ownership flag for cleanup.
                                let value = unsafe { &*pointer };
                                std::hint::black_box(value.xltype);
                                if value.xltype & xlfn_sys::XLBIT_DLL_FREE != 0 {
                                    // SAFETY: The live root requests one owning cleanup.
                                    let _ =
                                        unsafe { crate::return_abi::free_return_boundary(pointer) };
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
            let _ = crate::panic_boundary::contain_panic(worker.join());
        }
        if matches!(
            crate::module_runtime::ingress().phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            crate::module_runtime::ingress().begin_close_with(|| {});
            let _ = crate::module_runtime::ingress().seal_and_drain();
        }
    }
}

// ---------------------------------------------------------------------------
