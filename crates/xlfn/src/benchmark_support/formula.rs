use super::*;

// Semantic and UTF-16 input-identity benchmarks
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub trait BenchmarkInputIdentity {
    fn encode_identity(&self, encoder: &mut crate::input_identity::InputIdentityEncoder);
}

impl BenchmarkInputIdentity for f64 {
    fn encode_identity(&self, encoder: &mut crate::input_identity::InputIdentityEncoder) {
        encoder.f64(*self);
    }
}

impl BenchmarkInputIdentity for String {
    fn encode_identity(&self, encoder: &mut crate::input_identity::InputIdentityEncoder) {
        encoder.string(self);
    }
}

impl<T: BenchmarkInputIdentity> BenchmarkInputIdentity for Matrix<T> {
    fn encode_identity(&self, encoder: &mut crate::input_identity::InputIdentityEncoder) {
        encoder.u64(self.rows() as u64);
        encoder.u64(self.columns() as u64);
        for value in self.as_slice() {
            value.encode_identity(encoder);
        }
    }
}

fn fingerprint_argument<T: BenchmarkInputIdentity>(value: &T) -> [u8; 32] {
    let mut builder = crate::input_identity::InputFingerprintBuilder::new(1);
    builder
        .with_argument(0, "benchmark", |encoder| {
            value.encode_identity(encoder);
            Ok(())
        })
        .expect("benchmark semantic argument must fingerprint successfully");
    *builder
        .finish()
        .expect("benchmark fingerprint framing must be complete")
        .as_bytes()
}

pub struct SemanticIdentityBenchmark<T> {
    value: T,
}

/// Measures the semantic UTF-16 path used by prepared string inputs.
pub struct Utf16IdentityBenchmark {
    units: Vec<u16>,
}

impl Utf16IdentityBenchmark {
    pub fn new(value: &str) -> Self {
        Self {
            units: value.encode_utf16().collect(),
        }
    }

    pub fn run(&self) -> [u8; 32] {
        let mut builder = crate::input_identity::InputFingerprintBuilder::new(1);
        builder
            .with_argument(0, "benchmark", |encoder| {
                encoder.semantic_utf16(std::hint::black_box(&self.units))
            })
            .unwrap();
        *builder.finish().unwrap().as_bytes()
    }
}

impl<T> SemanticIdentityBenchmark<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }

    pub fn run(&self) -> [u8; 32]
    where
        T: BenchmarkInputIdentity,
    {
        fingerprint_argument(&self.value)
    }

    pub fn run_arguments(&self, count: usize) -> [u8; 32]
    where
        T: BenchmarkInputIdentity,
    {
        let mut builder = crate::input_identity::InputFingerprintBuilder::new(count);
        for index in 0..count {
            builder
                .with_argument(index, "benchmark", |encoder| {
                    self.value.encode_identity(encoder);
                    Ok(())
                })
                .unwrap();
        }
        *builder.finish().unwrap().as_bytes()
    }
}
