//! Concurrent caller traffic through the real scalar async launch boundary.
use super::{RawArgumentIngressBenchmark, get_benchmark_runtime};
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value};

// Only unexpected callback values write this flag, avoiding a successful-path
// global counter that would add unrelated contention to the measurement.
static CALLBACK_FAILED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn callback(
    function: i32,
    count: i32,
    arguments: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32 {
    if function != xlfn_sys::XL_ASYNC_RETURN
        || count != 2
        || arguments.is_null()
        || result.is_null()
    {
        CALLBACK_FAILED.store(true, Ordering::Release);
        return xlfn_sys::XLRET_FAILED;
    }
    // SAFETY: xlAsyncReturn provides two live pointer slots for this callback.
    let value_slot = unsafe { arguments.add(1) };
    // SAFETY: the second pointer slot is live throughout this callback.
    let value_pointer = unsafe { *value_slot };
    // SAFETY: the production wrapper retains its result value during the call.
    let value = unsafe { value_pointer.as_ref() };
    let successful_value = value.is_some_and(|value| {
        if value.base_type() != xlfn_sys::XLTYPE_NUM {
            return false;
        }
        // SAFETY: XLTYPE_NUM selects the numeric union field.
        let number = unsafe { value.value.number };
        number == 42.0
    });
    if !successful_value {
        CALLBACK_FAILED.store(true, Ordering::Release);
    }
    // SAFETY: the production wrapper supplies writable callback result storage.
    unsafe {
        *result = XLOPER12::boolean(true);
    }
    xlfn_sys::XLRET_SUCCESS
}

/// Owns persistent callers; their raw input fixtures never cross threads.
///
/// This fixture is exclusive: no other ABI-probe benchmark may replace the
/// process-wide callback or use the shared benchmark runtime concurrently.
/// Construction completes every caller's input setup before timing begins.
pub struct ConcurrentAsyncAdmissionBenchmark {
    runtime: &'static crate::runtime::Runtime<()>,
    start_tx: Vec<SyncSender<usize>>,
    done_rx: Receiver<usize>,
    batch_start: Arc<Barrier>,
    callers: Vec<JoinHandle<()>>,
}

impl ConcurrentAsyncAdmissionBenchmark {
    pub fn new(caller_count: usize) -> Self {
        assert!((1..=32).contains(&caller_count));
        crate::module_runtime::reset_callbacks_for_test();
        // SAFETY: callback has the Excel ABI and remains process-live.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                callback as *const () as *mut std::ffi::c_void,
            );
        }
        CALLBACK_FAILED.store(false, Ordering::Release);
        let runtime = get_benchmark_runtime();
        runtime.start_async(4).expect("benchmark workers start");
        assert!(runtime.async_manager().wait_idle());
        let batch_start = Arc::new(Barrier::new(caller_count + 1));
        let (ready_tx, ready_rx) = sync_channel(caller_count);
        let (done_tx, done_rx) = sync_channel(caller_count);
        let mut start_tx = Vec::with_capacity(caller_count);
        let mut callers = Vec::with_capacity(caller_count);
        for _ in 0..caller_count {
            let (caller_tx, caller_rx) = sync_channel::<usize>(1);
            let ready_tx = ready_tx.clone();
            let done_tx = done_tx.clone();
            let batch_start = Arc::clone(&batch_start);
            start_tx.push(caller_tx);
            callers.push(std::thread::spawn(move || {
                // Construct the raw-pointer-bearing input on this caller.
                // Neither this value nor its raw pointer leaves the thread.
                let mut input = RawArgumentIngressBenchmark::number(42.0);
                ready_tx.send(()).expect("driver waits for caller setup");
                while let Ok(calls) = caller_rx.recv() {
                    batch_start.wait();
                    let preparations = run_scalar_launches(runtime, &mut input, calls);
                    done_tx
                        .send(preparations)
                        .expect("driver receives caller completion");
                }
            }));
        }
        drop(ready_tx);
        drop(done_tx);
        for _ in 0..caller_count {
            ready_rx.recv().expect("each caller completes setup");
        }
        Self {
            runtime,
            start_tx,
            done_rx,
            batch_start,
            callers,
        }
    }

    pub fn run_and_drain(&self, calls_per_caller: usize) -> usize {
        let total_calls = self
            .start_tx
            .len()
            .checked_mul(calls_per_caller)
            .expect("benchmark batch size fits usize");
        // The driver drains each previous batch, so even if no task finishes
        // while callers submit, this batch cannot exhaust the 4,096-task limit.
        assert!(total_calls < 4096);
        for start in &self.start_tx {
            start
                .send(calls_per_caller)
                .expect("persistent caller receives a batch");
        }
        // All callers receive their work before a single batch release. This
        // prevents serial signal delivery from hiding caller contention.
        self.batch_start.wait();
        let mut preparations = 0;
        for _ in 0..self.start_tx.len() {
            preparations += self
                .done_rx
                .recv()
                .expect("every caller finishes boundary launches");
        }
        assert!(self.runtime.async_manager().wait_idle());
        assert_eq!(preparations, total_calls);
        assert!(
            !CALLBACK_FAILED.load(Ordering::Acquire),
            "all calls must deliver the scalar result without admission errors"
        );
        preparations
    }
}

fn run_scalar_launches(
    runtime: &'static crate::runtime::Runtime<()>,
    input: &mut RawArgumentIngressBenchmark,
    calls: usize,
) -> usize {
    let mut preparations = 0;
    for _ in 0..calls {
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: std::ptr::null_mut(),
                    },
                    byte_count: 0,
                },
            },
            xltype: xlfn_sys::XLTYPE_BIG_DATA,
        };
        // SAFETY: the raw handle is live and aligned during launch; the
        // responder copies the opaque token without dereferencing it.
        unsafe {
            crate::async_udf::async_udf_boundary_named(
                runtime,
                "BENCH.ADMISSION.CONCURRENT",
                "BENCH.ADMISSION.CONCURRENT",
                &mut raw,
                |_, _, _| {
                    preparations += 1;
                    input.run_plain::<f64>();
                    Ok(std::future::ready(Ok::<_, crate::XllError>(42.0)))
                },
            );
        }
    }
    preparations
}

impl Drop for ConcurrentAsyncAdmissionBenchmark {
    fn drop(&mut self) {
        // No launch/input state outlives a caller. Stop and join all callers
        // before shutting down the executor shared by this fixture.
        self.start_tx.clear();
        for caller in self.callers.drain(..) {
            crate::panic_boundary::contain_panic(caller.join()).expect("benchmark caller panicked");
        }
        assert!(self.runtime.async_manager().close().issues.is_empty());
    }
}
