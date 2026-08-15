use crate::{DomainErrorCode, ExcelCallbackStatus, ReturnContext, XllError, XllResult};
use arc_swap::ArcSwap;
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::cell::Cell;
use std::fmt::Write as _;
use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::thread::ThreadId;

mod connection;
mod formula;
mod lease;
#[cfg(any(test, feature = "handle-refinement-trace"))]
mod refinement;
mod registry;
mod runtime;
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) mod snapshot_refinement;
mod token;
mod typed;

pub(crate) use connection::*;
pub(crate) use formula::*;
pub(crate) use lease::*;
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) use refinement::{HandleRefinementTrace, TokenWire};
pub(crate) use registry::*;
pub(crate) use runtime::*;
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) use snapshot_refinement::{
    Event as SnapshotEvent, LeaseLineageTrace, SnapshotTokenWire, SnapshotTraceRecorder,
};
pub(crate) use token::*;
pub use typed::{ExcelHandleObject, Handle};

#[cfg(test)]
mod refinement_tests;
#[cfg(test)]
mod snapshot_trace_tests;
#[cfg(test)]
mod tests;
