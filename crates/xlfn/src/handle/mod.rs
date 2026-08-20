use crate::{DomainErrorCode, ExcelCallbackStatus, ReturnContext, XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::any::{TypeId, type_name};
use std::cell::Cell;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::thread::ThreadId;

mod connection;
mod formula;
mod prepare;
#[cfg(any(test, feature = "handle-refinement-trace"))]
mod refinement;
mod registry;
mod runtime;
mod token;
mod typed;

pub(crate) use connection::*;
pub(crate) use formula::*;
pub(crate) use prepare::*;
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) use refinement::{HandleRefinementTrace, TokenWire};
pub(crate) use registry::*;
pub(crate) use runtime::*;
pub(crate) use token::*;
pub use typed::{ExcelHandleObject, Handle, HandleAlias};

#[cfg(test)]
mod refinement_tests;
#[cfg(test)]
mod tests;
