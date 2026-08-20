#![allow(unsafe_code, reason = "Low-level FFI interaction for async UDF tasks")]
#![cfg(feature = "async")]

use crate::cancellation::CancellationSource;
use crate::execution::{CallId, CallMetadata, CallOutcome, UdfResultKind};
use crate::return_value::AsyncReturnPointer;
use crate::{
    CancellationGuarantee, CancellationToken, ExcelReturn, ReturnContext, Runtime, XllError,
    XllResult,
};
use arc_swap::{ArcSwapAny, ArcSwapOption};
use async_channel::{Receiver, Sender};
use async_task::Runnable;
use futures_util::FutureExt;
use futures_util::future::{AbortHandle, Abortable};
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
use xlfn_sys::{
    XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA, XLTYPE_BOOL,
};

mod boundary;
mod excel_handle;
mod executor;
mod generation;
mod manager;
mod task;
mod worker;

pub use boundary::*;
pub(crate) use excel_handle::*;
pub(crate) use executor::*;
pub(crate) use generation::*;
pub(crate) use manager::*;
pub(crate) use task::*;
pub(crate) use worker::*;

#[cfg(test)]
mod tests;
