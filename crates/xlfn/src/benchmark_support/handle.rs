use super::*;

// Handle prepare benchmarks
// ---------------------------------------------------------------------------

pub struct BenchHandleObject {
    pub _payload: u64,
}
impl ExcelHandleObject for BenchHandleObject {}

pub(super) fn benchmark_revision_key(udf_id: &'static str, id: u64) -> HandleTopicKey {
    let mut inputs = [0_u8; 32];
    inputs[..8].copy_from_slice(&id.to_le_bytes());
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        FormulaCaller {
            sheet_id: 1,
            row: 0,
            column: 0,
        },
        udf_id,
        InputFingerprint::from_bytes(inputs),
    ))
}

pub(super) fn cleanup_handle_runtime(runtime: &FormulaHandleService) {
    runtime.terminate_all_topics();
    let _ = runtime.seal();
}

/// A batch whose runtime and formula keys are prepared before the timed call.
pub struct HandleColdBatch {
    runtime: Arc<FormulaHandleService>,
    keys: Vec<HandleTopicKey>,
}

impl HandleColdBatch {
    pub fn new(iterations: usize) -> Self {
        Self {
            runtime: Arc::new(
                FormulaHandleService::try_new(iterations.max(1))
                    .expect("benchmark host provides an OS CSPRNG"),
            ),
            keys: (0..iterations)
                .map(|i| benchmark_revision_key("BENCH.COLD", i as u64))
                .collect(),
        }
    }

    pub fn run(&mut self) {
        let keys = std::mem::take(&mut self.keys);
        for (i, key) in keys.into_iter().enumerate() {
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("cold handle publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleColdBatch {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A warm-hit benchmark with its seed publication outside the timed section.
pub struct HandleWarmBenchmark {
    runtime: Arc<FormulaHandleService>,
    key: HandleTopicKey,
}

impl HandleWarmBenchmark {
    pub fn new() -> Self {
        let runtime = Arc::new(
            FormulaHandleService::try_new(1).expect("benchmark host provides an OS CSPRNG"),
        );
        let key = benchmark_revision_key("BENCH.WARM", 0);
        runtime
            .prepare_observed(key, || Ok(BenchHandleObject { _payload: 0 }), |_, _| Ok(()))
            .expect("warm handle seed publication failed");
        Self { runtime, key }
    }

    pub fn run(&self, iterations: usize) {
        for _ in 0..iterations {
            let result = self
                .runtime
                .prepare_observed(
                    self.key,
                    || -> crate::XllResult<BenchHandleObject> {
                        unreachable!("warm factory must not run")
                    },
                    |_, _| Ok(()),
                )
                .expect("warm handle observation failed");
            std::hint::black_box(result);
        }
    }
}

impl Default for HandleWarmBenchmark {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HandleWarmBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A cold-growth benchmark that inserts `N` unique topic keys into a single runtime.
pub struct HandleColdGrowthBenchmark {
    runtime: Arc<FormulaHandleService>,
    keys: Vec<HandleTopicKey>,
}

impl HandleColdGrowthBenchmark {
    pub fn new(count: usize) -> Self {
        Self {
            runtime: Arc::new(
                FormulaHandleService::try_new(count.max(1))
                    .expect("benchmark host provides an OS CSPRNG"),
            ),
            keys: (0..count)
                .map(|i| benchmark_revision_key("BENCH.COLD_GROW", i as u64))
                .collect(),
        }
    }

    pub fn run(&self) {
        for (i, &key) in self.keys.iter().enumerate() {
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("cold handle growth publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleColdGrowthBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

/// A revision-churn benchmark that repeatedly updates the same `N` topics with new objects.
pub struct HandleRevisionChurnBenchmark {
    runtime: Arc<FormulaHandleService>,
    keys: Vec<HandleTopicKey>,
    churn_cycles: usize,
}

impl HandleRevisionChurnBenchmark {
    pub fn new(topics: usize, churn_cycles: usize) -> Self {
        let runtime = Arc::new(
            FormulaHandleService::try_new(topics.max(1))
                .expect("benchmark host provides an OS CSPRNG"),
        );
        let keys: Vec<_> = (0..topics)
            .map(|i| benchmark_revision_key("BENCH.CHURN", i as u64))
            .collect();
        for (i, &key) in keys.iter().enumerate() {
            runtime
                .prepare_observed(
                    key,
                    || Ok(BenchHandleObject { _payload: i as u64 }),
                    |_, _| Ok(()),
                )
                .expect("initial seed publication failed");
        }
        Self {
            runtime,
            keys,
            churn_cycles,
        }
    }

    pub fn run(&self) {
        for cycle in 0..self.churn_cycles {
            let key = self.keys[cycle % self.keys.len()];
            let result = self
                .runtime
                .prepare_observed(
                    key,
                    || {
                        Ok(BenchHandleObject {
                            _payload: cycle as u64,
                        })
                    },
                    |_, _| Ok(()),
                )
                .expect("revision churn publication failed");
            std::hint::black_box(result);
        }
    }
}

impl Drop for HandleRevisionChurnBenchmark {
    fn drop(&mut self) {
        cleanup_handle_runtime(&self.runtime);
    }
}

// ---------------------------------------------------------------------------
