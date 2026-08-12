use crate::{DomainErrorCode, ExcelCallbackStatus, ReturnContext, XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::cell::Cell;
use std::fmt::Write as _;
use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
#[cfg(any(target_os = "windows", test))]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::ThreadId;

mod connection;
mod formula;
mod lease;
mod registry;
mod runtime;
mod token;
mod typed;

pub(crate) use connection::*;
pub(crate) use formula::*;
pub(crate) use lease::*;
pub(crate) use registry::*;
pub(crate) use runtime::*;
pub(crate) use token::*;
pub use typed::{ExcelHandleObject, Handle};

#[cfg(test)]
mod tests;
