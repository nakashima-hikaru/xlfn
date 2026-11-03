use super::*;

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

    let references = std::ptr::from_ref(&BENCH_CALLER_REFERENCES).cast_mut();
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
        crate::module_runtime::reset_callbacks_for_test();
        // SAFETY: `benchmark_formula_callback` has Excel's exact callback ABI
        // and remains live for the duration of this benchmark process.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                benchmark_formula_callback as *const () as *mut std::ffi::c_void,
            );
        }

        let callbacks = HostCallbackSession::new();
        let caller = resolve_formula_caller(crate::host_api::ExcelHost::new(&callbacks))
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
        let caller = resolve_formula_caller(crate::host_api::ExcelHost::new(&self.callbacks))
            .expect("benchmark callback must resolve a single-cell caller");
        (caller.sheet_id, caller.row, caller.column)
    }
}

/// Persistent workers exercising caller resolution through production module
/// admission. Each worker owns its own invocation-local callback session.
pub struct FormulaCallerWorkerPool {
    start_tx: Vec<std::sync::mpsc::SyncSender<()>>,
    start: Arc<std::sync::Barrier>,
    done_rx: std::sync::mpsc::Receiver<()>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl FormulaCallerWorkerPool {
    pub fn new(workers: usize, iterations: usize, case: FormulaCallerBenchCase) -> Self {
        Self::build(workers, iterations, case, false)
    }

    pub fn dispatch_only(workers: usize) -> Self {
        Self::build(workers, 0, FormulaCallerBenchCase::Ref, true)
    }

    fn build(
        workers: usize,
        iterations: usize,
        case: FormulaCallerBenchCase,
        dispatch_only: bool,
    ) -> Self {
        assert!(workers != 0);
        // Install the process-global deterministic stub once, before any
        // worker can acquire a module callback permit.
        let _ = FormulaCallerBenchmark::new(case);
        let ready = Arc::new(std::sync::Barrier::new(workers + 1));
        let start = Arc::new(std::sync::Barrier::new(workers + 1));
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(workers);
        let mut start_tx = Vec::with_capacity(workers);
        let mut threads = Vec::with_capacity(workers);
        for _ in 0..workers {
            let (worker_tx, worker_rx) = std::sync::mpsc::sync_channel::<()>(1);
            start_tx.push(worker_tx);
            let done = done_tx.clone();
            let ready = Arc::clone(&ready);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                let benchmark = FormulaCallerBenchmark {
                    callbacks: HostCallbackSession::new(),
                };
                let expected_sheet = match case {
                    FormulaCallerBenchCase::Ref => 17,
                    FormulaCallerBenchCase::SRef => 19,
                };
                assert_eq!(benchmark.run(), (expected_sheet, 11, 3));
                ready.wait();
                while worker_rx.recv().is_ok() {
                    start.wait();
                    if !dispatch_only {
                        for _ in 0..iterations {
                            std::hint::black_box(benchmark.run());
                        }
                    }
                    done.send(()).unwrap();
                }
            }));
        }
        ready.wait();
        Self {
            start_tx,
            start,
            done_rx,
            workers: threads,
        }
    }

    /// Batch timing includes dispatch, a simultaneous-start barrier, caller
    /// resolution, callback cleanup, and completion coordination.
    pub fn run_batch(&self) {
        for worker in &self.start_tx {
            worker.send(()).unwrap();
        }
        self.start.wait();
        for _ in 0..self.workers.len() {
            self.done_rx.recv().unwrap();
        }
    }
}

impl Drop for FormulaCallerWorkerPool {
    fn drop(&mut self) {
        self.start_tx.clear();
        for worker in self.workers.drain(..) {
            let _ = crate::panic_boundary::contain_panic(worker.join());
        }
    }
}

// ---------------------------------------------------------------------------
