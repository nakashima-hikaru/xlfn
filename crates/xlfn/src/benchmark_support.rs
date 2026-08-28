//! Benchmark support utilities for internal crate testing and performance measurement.
//!
//! This module is hidden from public API documentation and is enabled only when
//! compiling with the `bench-internals` feature.

#![cfg(feature = "bench-internals")]
#![doc(hidden)]
#![allow(unsafe_code, reason = "Benchmark-only XLOPER12 pointer construction")]

#[cfg(feature = "async")]
use crate::XllError;
#[cfg(feature = "async")]
use crate::async_udf::AsyncManager;
#[cfg(feature = "async")]
use crate::cancellation::CancellationGuarantee;
#[cfg(feature = "async")]
use crate::cancellation::CancellationSource;
use crate::value::{ExcelParameter, Matrix};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

/// Shared Criterion measurement policy for all production-path benchmarks.
pub const BENCHMARK_MEASUREMENT_TIME: Duration = Duration::from_secs(10);

use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaHandleService, FormulaRevisionKey, HandleTopicKey,
    resolve_formula_caller,
};
use crate::host_callback::HostCallbackSession;
use crate::input_identity::InputFingerprint;

mod async_spawn;
mod call_resolution;
mod formula;
mod formula_caller;
mod handle;
mod ingress;
mod lookup;
#[cfg(feature = "rtd")]
mod rtd;
mod sync_boundary;

#[cfg(feature = "async")]
pub use async_spawn::{AsyncSpawnBenchmark, AsyncSpawnKind, RescheduleFuture, SpawnBatchResult};
pub use call_resolution::{ConcurrentHandleResolutionBenchmark, MultiHandleCallBenchmark};
pub use formula::{BenchmarkInputIdentity, FormulaRevisionBenchmark, SemanticIdentityBenchmark};
pub use formula_caller::{FormulaCallerBenchCase, FormulaCallerBenchmark};
pub use handle::{
    BenchHandleObject, HandleColdBatch, HandleColdGrowthBenchmark, HandleRevisionChurnBenchmark,
    HandleWarmBenchmark,
};
pub use ingress::RawArgumentIngressBenchmark;
pub use lookup::{
    ArcHandleLookupBenchmark, HandleDistinctKeyBenchmark, HandleLookupBenchCase,
    HandleLookupBenchmark,
};
#[cfg(feature = "rtd")]
pub use rtd::{RtdPublishNumberBenchmark, RtdPublishStringBenchmark};
pub use sync_boundary::{SyncBenchKind, SyncBoundaryWorkerPool};

pub(super) fn get_benchmark_runtime() -> &'static crate::runtime::Runtime<()> {
    static RUNTIME: std::sync::OnceLock<crate::runtime::Runtime<()>> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        let runtime = crate::runtime::Runtime::new();
        runtime.arm_test_generation();
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
