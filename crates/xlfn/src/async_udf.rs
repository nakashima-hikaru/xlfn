#![allow(unsafe_code, reason = "Low-level FFI interaction for async UDF tasks")]
#![cfg(feature = "async")]

mod boundary;
mod completion;
mod excel_handle;
mod executor;
mod future;
mod generation;
mod instrumentation;
mod manager;
mod queue;
mod task;
mod worker;

#[cfg(feature = "handles")]
pub(crate) use boundary::async_udf_boundary_named_handle;
pub(crate) use boundary::{
    async_udf_boundary_named, cancel_async_calculation, end_async_calculation,
};
#[cfg(feature = "handles")]
pub use executor::{AsyncTaskScope, HandleScopedBuilder};
#[cfg(all(test, feature = "handles"))]
pub(crate) use executor::{HandleScopedTaskBuilder, ScopedTaskFuture};
pub(crate) use manager::{AsyncManager, AsyncStopped};

#[cfg(all(feature = "bench-internals", feature = "handles"))]
pub use boundary::HandleScopedDeliveryBenchmark;
#[cfg(feature = "bench-internals")]
pub use generation::AsyncTaskDrainBenchmark;

// Test modules exercise the protocol pieces directly. Keep these imports
// scoped to tests so the production module has no ambient prelude.
#[cfg(test)]
#[cfg(test)]
mod tests;
