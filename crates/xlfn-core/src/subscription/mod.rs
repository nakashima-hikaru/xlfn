#![cfg_attr(
    not(target_os = "windows"),
    allow(dead_code, reason = "Internal helpers for Windows COM integration")
)]

use crate::{ExcelErrorValue, XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

mod catalog;
mod delivery;
mod identity;
mod operation_gate;
mod quota;
mod runtime;
mod server;
mod slot;
mod source;
mod topic;
mod value;

pub use source::{RtdSink, RtdSource, RtdSubscription};
pub use topic::{RtdLimits, RtdTopic};
pub use value::{IntoRtdValue, RtdValue};

pub(crate) use catalog::*;
pub(crate) use delivery::*;
pub(crate) use identity::*;
pub(crate) use operation_gate::*;
pub(crate) use quota::*;
pub(crate) use runtime::*;
pub(crate) use server::*;
pub(crate) use slot::*;
pub(crate) use source::ErasedRtdSource;
pub(crate) use topic::{
    ConnectionGeneration, ServerGeneration, SourceId, SubscriptionIdentity, SubscriptionKey,
    TopicId,
};
pub(crate) use value::StoredRtdValue;
#[cfg(test)]
pub(crate) mod tests;
