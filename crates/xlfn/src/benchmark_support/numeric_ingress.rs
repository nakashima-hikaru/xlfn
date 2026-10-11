//! Production FP12 argument ingress with the same call setup as generic ingress.

use super::{BenchmarkAddin, benchmark_ingress, get_benchmark_runtime};

/// Numeric-array fixture with Excel's FP12 layout and contiguous double payload.
pub struct NumericMatrixIngressBenchmark {
    runtime: &'static crate::runtime::Runtime<BenchmarkAddin>,
    // The first eight bytes hold the two i32 dimensions. Keeping Vec storage
    // lets the pointer be derived afresh after all owner moves have completed.
    storage: Vec<f64>,
}

impl NumericMatrixIngressBenchmark {
    /// Constructs a row-major matrix containing `0..rows * columns`.
    pub fn new(rows: usize, columns: usize) -> Self {
        let length = rows.checked_mul(columns).expect("benchmark shape must fit");
        crate::value::validate_matrix_dimensions(rows, columns, length)
            .expect("benchmark matrix dimensions must be valid");
        let mut storage = Vec::with_capacity(length + 1);
        storage.push(0.0);
        storage.extend((0..length).map(|index| index as f64));
        let header = storage.as_mut_ptr().cast::<i32>();
        // SAFETY: the first aligned eight-byte slot is allocated and uniquely
        // owned. FP12's two i32 header fields precede the initialized payload.
        unsafe { header.write(i32::try_from(rows).unwrap()) };
        // SAFETY: the second four-byte header field is within the uniquely
        // owned, aligned eight-byte slot allocated above.
        unsafe {
            header
                .wrapping_add(1)
                .write(i32::try_from(columns).unwrap())
        };
        Self {
            runtime: get_benchmark_runtime(),
            storage,
        }
    }

    /// Validates and borrows the numeric buffer through production call setup.
    pub fn run_plain(&mut self) {
        let ingress = benchmark_ingress();
        let call = self
            .runtime
            .enter(&ingress)
            .expect("benchmark runtime must be open");
        crate::call::with_excel_call_scope_and_call(&call, |call, scope| {
            let mut arguments =
                crate::value::ArgumentContext::<crate::value::PlainInputMode>::new(call, scope, 1);
            // SAFETY: storage has the FP12 header and initialized contiguous
            // doubles and is not mutated during this benchmark call.
            let value = unsafe {
                crate::value::NumericMatrixRef::from_raw(self.storage.as_ptr().cast(), "arg")
            }
            .expect("numeric benchmark input must be valid");
            arguments.record_numeric_matrix(0, "arg", value).unwrap();
            std::hint::black_box(value);
            let _ = arguments.finish();
        });
    }
}
