//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]
#![allow(unsafe_code, reason = "Benchmark-only XLOPER12 pointer construction")]

#[cfg(feature = "async-builtin")]
use crate::XllError;
#[cfg(feature = "async-builtin")]
use crate::async_udf::AsyncRuntime;
#[cfg(feature = "async-builtin")]
use crate::cancellation::CancellationGuarantee;
#[cfg(feature = "async-builtin")]
use crate::cancellation::CancellationSource;
use crate::value::{ExcelParameter, Matrix};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

/// Shared Criterion measurement policy for all production-path benchmarks.
pub const BENCHMARK_MEASUREMENT_TIME: Duration = Duration::from_secs(10);

/// Returns the standard measurement time, with an opt-in override for local
/// crossing-point exploration. CI and normal runs keep the ten-second policy.
pub fn benchmark_measurement_time() -> Duration {
    std::env::var("XLFN_BENCH_MEASUREMENT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|&milliseconds| milliseconds != 0)
        .map(Duration::from_millis)
        .unwrap_or(BENCHMARK_MEASUREMENT_TIME)
}

use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaHandleService, FormulaRevisionKey, HandleTopicKey,
    resolve_formula_caller,
};
use crate::host_callback::HostCallbackSession;
use crate::input_identity::InputFingerprint;

#[cfg(feature = "async-builtin")]
mod async_admission;
#[cfg(feature = "async-builtin")]
mod async_admission_concurrent;
mod async_spawn;
#[cfg(feature = "cache")]
mod cache;
mod call_resolution;
mod formula;
mod formula_caller;
mod handle;
mod ingress;
mod lookup;
mod output;
#[cfg(feature = "rtd")]
mod rtd;
mod sync_boundary;

pub use crate::diagnostics::benchmark::{
    DiagnosticBenchCase, DiagnosticBenchmark, DiagnosticProbeResult,
};

pub use crate::handle::{
    handle_removal_probe, handle_retirement_debt_probe, token_cache_associativity_probe,
};
#[cfg(feature = "rtd")]
pub use crate::subscription::channel_protocol_probe;

#[cfg(feature = "async")]
pub use crate::handle::{ObjectFinalPinRelease, ObjectLeaseBenchCase, ObjectLeaseBenchmark};

#[cfg(feature = "async")]
pub use crate::async_udf::AsyncTaskDrainBenchmark;
#[cfg(all(feature = "async-builtin", feature = "handles"))]
pub use crate::async_udf::HandleScopedDeliveryBenchmark;
#[cfg(feature = "async")]
pub use crate::cancellation::benchmark::CancellationLifecycleBenchmark;
pub use crate::value::prepared_probe::InputKind as TwoPhaseInputKind;
#[cfg(feature = "async-builtin")]
pub use async_admission::AsyncAdmissionBenchmark;
#[cfg(feature = "async-builtin")]
pub use async_admission_concurrent::ConcurrentAsyncAdmissionBenchmark;
#[cfg(feature = "async-builtin")]
pub use async_spawn::{AsyncSpawnBenchmark, AsyncSpawnKind, RescheduleFuture, SpawnBatchResult};
#[cfg(feature = "cache")]
pub use cache::{
    CacheLookupBenchCase, CurrentCacheBenchmark, CurrentCacheEvictionBenchmark,
    RegistryCacheBenchmark,
};
pub use call_resolution::{ConcurrentHandleResolutionBenchmark, MultiHandleCallBenchmark};
pub use formula::{
    BenchmarkInputIdentity, FormulaRevisionBenchmark, SemanticIdentityBenchmark,
    Utf16IdentityBenchmark,
};
pub use formula_caller::{FormulaCallerBenchCase, FormulaCallerBenchmark, FormulaCallerWorkerPool};
pub use handle::{
    BenchHandleObject, HandleColdBatch, HandleColdGrowthBenchmark, HandleRevisionChurnBenchmark,
    HandleWarmBenchmark,
};
pub use ingress::{RawArgumentIngressBenchmark, TwoPhaseBenchmark};
pub use lookup::{HandleDistinctKeyBenchmark, HandleLookupBenchCase, HandleLookupBenchmark};
pub use output::{
    BorrowedStringArrayOutputBenchmark, NumericArrayOutputBenchmark, ScalarOutputBenchmark,
};
#[cfg(feature = "rtd")]
pub use rtd::{
    RTD_REFRESH_SCALING_CASES, RtdChannelPipelineBenchmark, RtdPrepareBenchmark,
    RtdPublishNumberBenchmark, RtdPublishStringBenchmark, RtdRefreshScalingBenchmark,
    RtdRefreshScalingCase, RtdRefreshValueKind, rtd_pipeline_probe,
};
pub use sync_boundary::{SyncBenchKind, SyncBoundaryWorkerPool};

pub(super) struct BenchmarkAddin;

impl crate::Addin for BenchmarkAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = crate::XllError;
    type Layers = ();
    #[cfg(feature = "async-builtin")]
    type AsyncExecutor = crate::BuiltinAsyncExecutor;
    #[cfg(all(feature = "async", not(feature = "async-builtin")))]
    type AsyncExecutor = crate::NoAsyncExecutor;

    fn open(_: &crate::OpenContext) -> crate::OpenResult<Self> {
        unreachable!("benchmark fixtures publish their isolated runtime directly")
    }
}

pub(super) fn get_benchmark_runtime() -> &'static crate::runtime::Runtime<BenchmarkAddin> {
    static RUNTIME: std::sync::OnceLock<crate::runtime::Runtime<BenchmarkAddin>> =
        std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        let runtime = crate::runtime::Runtime::new();
        let removal_epoch = runtime.removal_epoch();
        let opening = runtime
            .begin_open_if_epoch(removal_epoch)
            .expect("benchmark runtime open attempt");
        let mut opening = runtime.publish(opening, (), ());
        runtime
            .finish_open(&mut opening, Vec::new())
            .expect("benchmark runtime open");
        drop(opening);
        runtime
    })
}

pub(super) fn benchmark_ingress() -> crate::ingress::AdmittedExport<'static> {
    crate::module_runtime::ingress()
        .enter_with(|| {})
        .into_admitted()
        .expect("benchmark runtime ingress must be open")
}
