#![allow(unsafe_code, reason = "Low-level FFI interaction for async UDF tasks")]
#![cfg(feature = "async")]

//! Native Excel async transport, task lifecycle, and executor SPI.
//!
//! The safety contract is shared by every executor implementation:
//!
//! - A1: `TaskRegistry::active_tasks` is the sole authority for framework task
//!   lifetimes, including tasks never polled or already removed by cancellation.
//! - A2: executors receive only opaque `AsyncTask<F>` values; calculation state,
//!   cancellation, Excel response ownership, and handle services stay internal.
//! - A3: calculation owners are reclaimed only after their pin count reaches
//!   zero under calculation publication exclusion.
//! - A4: cancellation and Excel delivery retain the cancellation-source CAS
//!   linearization; executor scheduling does not decide their winner.
//! - A5: executor reservation precedes registry reservation. After registry
//!   commit, `submit` is an infallible ownership transfer.
//! - A6: `AsyncStopped` requires `active_tasks == 0`, a drained registry release
//!   tail, and successful executor shutdown. Errors never grant a certificate.
//! - A7: generation services remain live until that certificate; the runtime
//!   then stops subscription producers and seals handles before reclamation.
//!
//! An executor that retains a task forever can prevent closing, but cannot
//! permit the framework to reclaim a calculation or generation prematurely.

mod boundary;
#[cfg(feature = "async-builtin")]
mod builtin;
mod calculation;
mod completion;
mod executor;
mod future;
mod instrumentation;
mod registry;
mod runtime;
mod task;
#[cfg(feature = "handles")]
mod task_scope;
mod transport;

#[cfg(feature = "handles")]
pub(crate) use boundary::async_udf_boundary_named_handle;
pub(crate) use boundary::{
    async_udf_boundary_named, cancel_async_calculation, end_async_calculation,
};
#[cfg(feature = "async-builtin")]
pub use builtin::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};
#[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
pub(crate) use calculation::CalculationEpoch;
pub use executor::AsyncExecutor;
pub(crate) use runtime::{AsyncRuntime, AsyncStopped};
pub use task::AsyncTask;
#[cfg(feature = "handles")]
pub use task_scope::{AsyncTaskScope, HandleScopedBuilder};
#[cfg(all(test, feature = "handles", feature = "async-builtin"))]
pub(crate) use task_scope::{HandleScopedTaskBuilder, ScopedTaskFuture};

#[cfg(all(
    feature = "bench-internals",
    feature = "handles",
    feature = "async-builtin"
))]
pub use boundary::HandleScopedDeliveryBenchmark;
#[cfg(feature = "bench-internals")]
pub use calculation::AsyncTaskDrainBenchmark;

// Test modules exercise the protocol pieces directly. Keep these imports
// scoped to tests so the production module has no ambient prelude.
#[cfg(all(test, feature = "async-builtin"))]
mod tests;

#[cfg(test)]
mod external_tests;

#[cfg(test)]
mod snapshot_tests;

#[cfg(test)]
mod task_tests;
