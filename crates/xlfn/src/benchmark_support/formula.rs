use super::handle::cleanup_handle_runtime;
use super::*;

// Formula-to-handle end-to-end benchmarks
// ---------------------------------------------------------------------------

const HANDLE_FORMULA_UDF_ID: &str = "BENCH.HANDLE";

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
}

pub struct FormulaRevisionBenchmark<T> {
    runtime: Arc<FormulaHandleService>,
    argument: T,
    caller: FormulaCaller,
    factory_calls: AtomicUsize,
}

impl<T> FormulaRevisionBenchmark<T>
where
    T: BenchmarkInputIdentity,
{
    pub fn new(argument: T) -> Self {
        let caller = FormulaCaller {
            sheet_id: 7,
            row: 42,
            column: 11,
        };
        let runtime = Arc::new(
            FormulaHandleService::try_new(1).expect("benchmark host provides an OS CSPRNG"),
        );
        let factory_calls = AtomicUsize::new(0);
        let key = formula_revision_key(&argument, caller);

        runtime
            .prepare_observed(
                key,
                || {
                    factory_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(BenchHandleObject { _payload: 0 })
                },
                |_, _| Ok(()),
            )
            .expect("formula handle warm seed publication failed");
        assert_eq!(factory_calls.load(Ordering::Relaxed), 1);

        Self {
            runtime,
            argument,
            caller,
            factory_calls,
        }
    }

    pub fn run(&self) -> String {
        let key = formula_revision_key(&self.argument, self.caller);
        self.runtime
            .prepare_observed(
                key,
                || -> crate::XllResult<BenchHandleObject> {
                    self.factory_calls.fetch_add(1, Ordering::Relaxed);
                    panic!("formula handle warm-hit factory must not run");
                },
                |_, _| Ok(()),
            )
            .expect("formula handle warm observation failed")
            .into_token()
    }

    pub fn assert_warm_hit(&self) {
        assert_eq!(
            self.factory_calls.load(Ordering::Relaxed),
            1,
            "formula handle benchmark executed its factory during warm-hit measurement"
        );
    }
}

fn formula_revision_key<T>(arguments: &T, caller: FormulaCaller) -> HandleTopicKey
where
    T: BenchmarkInputIdentity,
{
    let inputs = fingerprint_argument(arguments);
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        caller,
        HANDLE_FORMULA_UDF_ID,
        InputFingerprint::from_bytes(inputs),
    ))
}

impl<T> Drop for FormulaRevisionBenchmark<T> {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

// ---------------------------------------------------------------------------
