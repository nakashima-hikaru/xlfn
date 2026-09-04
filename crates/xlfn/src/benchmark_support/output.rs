use crate::return_abi::XlArrayBuilder;

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
}
