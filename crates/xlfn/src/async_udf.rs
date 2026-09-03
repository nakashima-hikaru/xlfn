#![allow(unsafe_code, reason = "Low-level FFI interaction for async UDF tasks")]
#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test module"
    )
)]
#![cfg(feature = "async")]

mod boundary;
mod completion;
mod excel_handle;
mod executor;
mod generation;
mod instrumentation;
mod manager;
mod queue;
mod task;
mod worker;

pub(crate) use boundary::{
    async_udf_boundary_named, cancel_async_calculation, end_async_calculation,
};
pub(crate) use manager::{AsyncManager, AsyncStopped};

// Test modules exercise the protocol pieces directly. Keep these imports
// scoped to tests so the production module has no ambient prelude.
#[cfg(test)]
use crate::call_return::{ExcelReturn, ReturnContext};
#[cfg(test)]
use crate::cancellation::CancellationSource;
#[cfg(test)]
use crate::cancellation::{CancellationGuarantee, CancellationToken};
#[cfg(test)]
use crate::execution::{
    CallId, CallMetadata, CallOutcome, UdfCompletionOutcome, UdfDeliveryOutcome, UdfErrorKind,
};
#[cfg(test)]
use crate::return_abi::AsyncReturnPointer;
#[cfg(test)]
use crate::runtime::Runtime;
#[cfg(test)]
#[cfg(test)]
use crate::{XllError, XllResult};
#[cfg(test)]
use arc_swap::{ArcSwapAny, ArcSwapOption};
#[cfg(test)]
use async_task::Runnable;
#[cfg(test)]
pub(crate) use boundary::AFTER_ASYNC_EVALUATION_HOOK;
#[cfg(test)]
pub(crate) use excel_handle::ExcelAsyncResponder;
#[cfg(test)]
pub(crate) use executor::{Executor, ExecutorPtr, ExecutorShared};
#[cfg(test)]
use futures_util::FutureExt;
#[cfg(test)]
use futures_util::future::{AbortHandle, Abortable};
#[cfg(test)]
pub(crate) use generation::{
    ControlPhase, ExecutorControl, GenerationState, TaskShard, task_shard,
};
#[cfg(test)]
pub(crate) use manager::{ExecutorState, MAX_ASYNC_HANDLE_BYTES, MAX_PENDING};
#[cfg(test)]
use parking_lot::{Condvar, Mutex};
#[cfg(test)]
pub(crate) use queue::RunnableQueue;
#[cfg(test)]
use rustc_hash::FxHashMap;
#[cfg(test)]
use std::future::Future;
#[cfg(test)]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::thread::{self, JoinHandle};
#[cfg(test)]
pub(crate) use task::{ActiveReservation, CompletionGuard, TaskControl};
#[cfg(test)]
pub(crate) use worker::{
    WorkerExitGuard, cancel_source_no_unwind, cancel_tasks, cancelled_calculation_error,
    release_active, run_executor,
};
#[cfg(test)]
use xlfn_sys::{
    XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA, XLTYPE_BOOL,
};

#[cfg(test)]
mod tests;
