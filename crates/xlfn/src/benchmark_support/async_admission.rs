//! Full launch-boundary and owned input conversion fixture.
use super::{RawArgumentIngressBenchmark, get_benchmark_runtime};
use crate::cancellation::{CancellationGuarantee, CancellationSource};
use crate::value::Matrix;
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value};

unsafe extern "system" fn callback(
    function: i32,
    count: i32,
    _: *mut *mut XLOPER12,
    result: *mut XLOPER12,
) -> i32 {
    if function != xlfn_sys::XL_ASYNC_RETURN || count != 2 || result.is_null() {
        return xlfn_sys::XLRET_FAILED;
    }
    // SAFETY: the production wrapper supplies writable callback result storage.
    unsafe {
        *result = XLOPER12::boolean(true);
    }
    xlfn_sys::XLRET_SUCCESS
}

pub struct AsyncAdmissionBenchmark {
    runtime: &'static crate::runtime::Runtime<()>,
    input: RawArgumentIngressBenchmark,
    matrix: bool,
    saturated: bool,
    preparations: usize,
}

impl AsyncAdmissionBenchmark {
    pub fn new(matrix_elements: Option<usize>, saturated: bool) -> Self {
        crate::module_runtime::reset_callbacks_for_test();
        // SAFETY: callback has the Excel ABI and remains process-live.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                callback as *const () as *mut std::ffi::c_void,
            );
        }
        let runtime = get_benchmark_runtime();
        runtime.start_async(4).expect("benchmark workers start");
        if saturated {
            let generation = runtime.async_manager().current_generation();
            for _ in 0..4096 {
                let (source, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
                runtime
                    .async_manager()
                    .spawn(generation, std::future::pending(), source)
                    .expect("fill active task capacity");
            }
        }
        Self {
            runtime,
            input: matrix_elements.map_or_else(
                || RawArgumentIngressBenchmark::number(42.0),
                RawArgumentIngressBenchmark::number_vec,
            ),
            matrix: matrix_elements.is_some(),
            saturated,
            preparations: 0,
        }
    }

    pub fn run(&mut self, calls: usize) -> usize {
        let before = self.preparations;
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
            // SAFETY: raw is live and aligned throughout synchronous launch;
            // its opaque token is copied by the responder and never dereferenced.
            unsafe {
                crate::async_udf::async_udf_boundary_named(
                    self.runtime,
                    "BENCH.ADMISSION",
                    "BENCH.ADMISSION",
                    &mut raw,
                    |_, _, _| {
                        self.preparations += 1;
                        if self.matrix {
                            self.input.run_plain::<Matrix<f64>>();
                        } else {
                            self.input.run_plain::<f64>();
                        }
                        Ok(std::future::ready(Ok::<_, crate::XllError>(42.0)))
                    },
                );
            }
        }
        if !self.saturated {
            assert!(
                self.runtime.async_manager().wait_idle(),
                "submitted tasks drain"
            );
            assert_eq!(self.preparations - before, calls);
        }
        self.preparations - before
    }
}

impl Drop for AsyncAdmissionBenchmark {
    fn drop(&mut self) {
        assert!(self.runtime.async_manager().close().issues.is_empty());
    }
}
