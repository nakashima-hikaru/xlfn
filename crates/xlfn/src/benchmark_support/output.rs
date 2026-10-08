use crate::return_abi::XlArrayBuilder;

/// Scalar construction, primitive writer dispatch, return publication and
/// cleanup through the production synchronous boundary. Fixture text is
/// allocated before measurement; only the owned control clones it per call.
pub struct ScalarOutputBenchmark {
    runtime: &'static crate::runtime::Runtime<super::BenchmarkAddin>,
    text: String,
}

#[derive(Clone, Copy, crate::ExcelEnum)]
enum ScalarBenchmarkStatus {
    Ready,
}

impl ScalarOutputBenchmark {
    pub fn new(text: String) -> Self {
        Self {
            runtime: super::get_benchmark_runtime(),
            text,
        }
    }

    pub fn run_borrowed(&self) {
        self.run(self.text.as_str(), xlfn_sys::XLTYPE_STR);
    }

    pub fn run_owned(&self) {
        self.run(self.text.clone(), xlfn_sys::XLTYPE_STR);
    }

    pub fn run_enum(&self) {
        self.run(ScalarBenchmarkStatus::Ready, xlfn_sys::XLTYPE_STR);
    }

    pub fn run_number(&self) {
        self.run(std::hint::black_box(42.0), xlfn_sys::XLTYPE_NUM);
    }

    fn run<T: crate::call_return::ExcelReturn>(&self, value: T, expected_type: u32) {
        let pointer = crate::return_abi::udf_boundary_named(
            self.runtime,
            "bench_scalar_output",
            "BENCH.SCALAR.OUTPUT",
            |_, _| {
                let mut context = crate::call_return::ReturnContext::new();
                T::invoke(&mut context, || Ok(value))
            },
        );
        // SAFETY: the boundary returned a live return block or static error.
        let value_type = unsafe { (*pointer).base_type() };
        assert_eq!(
            value_type, expected_type,
            "scalar benchmark must encode successfully"
        );
        std::hint::black_box(pointer);
        // SAFETY: return this framework-owned allocation exactly once.
        let _ = unsafe { crate::return_abi::free_return_boundary(pointer) };
    }
}

/// Compares numerical result construction and the production synchronous
/// return boundary. Input fixtures are prepared before the measured operation;
/// both paths include result allocation, publication, and `xlAutoFree12` cleanup.
pub struct NumericArrayOutputBenchmark {
    runtime: &'static crate::runtime::Runtime<super::BenchmarkAddin>,
    values: Vec<f64>,
}

impl NumericArrayOutputBenchmark {
    pub fn new(cells: usize) -> Self {
        assert!(cells > 0, "benchmark array must be non-empty");
        let benchmark = Self {
            runtime: super::get_benchmark_runtime(),
            values: (0..cells).map(|index| index as f64 * 0.125).collect(),
        };
        benchmark.validate_result_paths();
        benchmark
    }

    fn result_value(value: f64) -> f64 {
        value * 1.25 + 0.5
    }

    /// Fixture setup for timing conversion of an already materialized result.
    pub fn prepared_matrix(&self) -> crate::value::Matrix<f64> {
        crate::value::Matrix::new(
            self.values.len(),
            1,
            std::hint::black_box(&self.values)
                .iter()
                .map(|&value| Self::result_value(value))
                .collect(),
        )
        .expect("benchmark array dimensions must be valid")
    }

    fn build_direct(&self) -> crate::XllResult<crate::return_abi::XlArrayOutput> {
        let mut builder = XlArrayBuilder::new(self.values.len(), 1)?;
        for &value in std::hint::black_box(&self.values) {
            builder.push(Self::result_value(value))?;
        }
        builder.finish()
    }

    pub fn run_matrix(&self) {
        self.run(|| Ok(self.prepared_matrix()));
    }

    pub fn run_direct(&self) {
        self.run(|| self.build_direct());
    }

    pub fn run_prepared_matrix(&self, matrix: crate::value::Matrix<f64>) {
        self.run(|| Ok(matrix));
    }

    fn run<T: crate::call_return::ExcelReturn>(
        &self,
        operation: impl FnOnce() -> crate::XllResult<T>,
    ) {
        let pointer = crate::return_abi::udf_boundary_named(
            self.runtime,
            "bench_array_output",
            "BENCH.ARRAY.OUTPUT",
            |_, _| {
                let mut context = crate::call_return::ReturnContext::new();
                T::invoke(&mut context, operation)
            },
        );
        // SAFETY: the boundary returned a live return block or static error.
        let value_type = unsafe { (*pointer).xltype & xlfn_sys::XLTYPE_MASK };
        assert_eq!(
            value_type,
            xlfn_sys::XLTYPE_MULTI,
            "benchmark must return an array rather than time an error path",
        );
        std::hint::black_box(pointer);
        // SAFETY: return this framework-owned allocation exactly once.
        let _ = unsafe { crate::return_abi::free_return_boundary(pointer) };
    }

    fn validate_result_paths(&self) {
        use crate::call_return::{ExcelReturn, ReturnContext, ReturnPayload};

        let matrix = self
            .prepared_matrix()
            .into_excel(&mut ReturnContext::new())
            .expect("benchmark matrix must encode");
        let ReturnPayload::Array(matrix) = matrix else {
            panic!("benchmark matrix must produce an array");
        };
        let direct = self
            .build_direct()
            .expect("benchmark direct array must encode");
        assert_eq!((matrix.rows, matrix.columns), (direct.rows, direct.columns));
        assert_eq!(matrix.cells.len(), direct.cells.len());
        for (index, (matrix, direct)) in matrix.cells.iter().zip(direct.cells.iter()).enumerate() {
            let expected = Self::result_value(self.values[index]);
            for cell in [matrix, direct] {
                assert_eq!(
                    crate::value::XlValueRef::from_array_cell(cell)
                        .expect("benchmark output cell must be valid")
                        .as_f64()
                        .expect("benchmark output cell must be numeric"),
                    expected,
                );
            }
        }
    }
}

pub struct BorrowedStringArrayOutputBenchmark {
    cells: usize,
    payload: String,
}

impl BorrowedStringArrayOutputBenchmark {
    pub fn new(cells: usize, payload_len: usize) -> Self {
        assert!(cells > 0, "benchmark array must be non-empty");
        assert!(payload_len > 0, "benchmark payload length must be non-zero");
        Self {
            cells,
            payload: "x".repeat(payload_len),
        }
    }

    pub fn with_payload(cells: usize, payload: String) -> Self {
        assert!(cells > 0, "benchmark array must be non-empty");
        Self { cells, payload }
    }

    #[inline]
    pub fn run_borrowed(&self) {
        let mut builder =
            XlArrayBuilder::new(self.cells, 1).expect("benchmark array dimensions must be valid");
        for _ in 0..self.cells {
            builder
                .push(self.payload.as_str())
                .expect("borrowed string output must encode");
        }
        std::hint::black_box(builder.finish().expect("benchmark array must finish"));
    }

    #[inline]
    pub fn run_counted_utf16(&self) {
        std::hint::black_box(
            crate::utf16::encode_counted(
                &self.payload,
                "benchmark",
                crate::utf16::EXCEL_STRING_LIMIT,
            )
            .expect("benchmark callback string must encode"),
        );
    }
}
