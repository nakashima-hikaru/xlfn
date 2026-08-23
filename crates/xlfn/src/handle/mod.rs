#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test modules"
    )
)]

mod binding;
mod connection;
mod formula;
mod object;
mod prepare;
#[cfg(any(test, feature = "refinement"))]
mod refinement;
mod refinement_hooks;
mod refinement_wire;
mod registry;
mod runtime;
mod store;
mod token;
mod topic;
mod typed;

#[cfg(test)]
use crate::error::DomainErrorCode;
#[cfg(test)]
pub(crate) use crate::generation::{BindingGeneration, TopicGeneration};
#[cfg(test)]
use crate::return_value::{ExcelCallbackStatus, ReturnContext};
#[cfg(test)]
use crate::{XllError, XllResult};
#[cfg(test)]
use parking_lot::{Condvar, Mutex, RwLock};
#[cfg(test)]
use rustc_hash::FxHashMap;
#[cfg(test)]
use std::any::{TypeId, type_name};
#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::fmt::Write as _;
#[cfg(test)]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

#[cfg(test)]
pub(crate) use binding::BindingState;
#[cfg(any(target_os = "windows", test))]
pub(crate) use connection::HandleConnection;
pub(crate) use connection::{FormulaBinding, HandleTopicOwner, Topic};
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::FormulaCaller;
#[cfg(any(test, feature = "refinement"))]
pub(crate) use formula::FormulaRevisionKey;
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::resolve_formula_caller;
#[cfg(test)]
pub(crate) use formula::test_topic_key;
pub(crate) use formula::{HandleTopicKey, formula_revision_key};
pub(crate) use object::SharedObject;
pub(crate) use prepare::HandlePrepareState;
#[cfg(target_os = "windows")]
pub(crate) use prepare::RtdOperationGuard;
pub(crate) use refinement_hooks::HandleRefinementHooks;
pub(crate) use refinement_wire::TokenWire;
#[cfg(test)]
pub(crate) use registry::HandleRegistryPhase;
pub(crate) use registry::HandleRegistrySealed;
#[cfg(test)]
pub(crate) use registry::{HandleRegistry, PendingHandleValue};
pub(crate) use runtime::{
    FormulaHandleService, FormulaHandleServiceResolver, FormulaHandleServiceSealed,
    FormulaHandleServiceSlot, HandleStoreQuiescent,
};
pub(crate) use store::HandleStore;
pub(crate) use token::{HandleId, HandleToken, ObjectId};
pub(crate) use topic::{
    Initialization, PrepareDecision, PublishedTopic, PublishedTopicState, TopicRemoval, TopicTable,
};
pub use typed::{ExcelHandleObject, Handle, HandleAlias, HandleLease, HandleObjectId};

#[cfg(test)]
mod refinement_tests;
#[cfg(test)]
mod tests;
