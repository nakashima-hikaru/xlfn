#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test modules"
    )
)]

mod arena;
mod connection;
mod formula;
mod prepare;
mod reclamation;
#[cfg(any(test, feature = "handle-refinement-trace"))]
mod refinement;
mod refinement_hooks;
mod refinement_wire;
mod registry;
mod runtime;
mod token;
mod topic;
mod typed;

#[cfg(test)]
use crate::error::DomainErrorCode;
#[cfg(test)]
pub(crate) use crate::generation::{BindingGeneration, ObjectGeneration, TopicGeneration};
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

pub(crate) use arena::{BorrowedObject, PinnedObject};
#[cfg(any(target_os = "windows", test))]
pub(crate) use connection::HandleConnection;
pub(crate) use connection::{FormulaBinding, HandleTopicOwner, Topic};
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::FormulaCaller;
#[cfg(any(test, feature = "bench-internals", feature = "handle-refinement-trace"))]
pub(crate) use formula::FormulaRevisionKey;
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::resolve_formula_caller;
#[cfg(test)]
pub(crate) use formula::test_topic_key;
pub(crate) use formula::{HandleTopicKey, formula_revision_key};
pub(crate) use prepare::HandlePrepareState;
#[cfg(target_os = "windows")]
pub(crate) use prepare::RtdOperationGuard;
pub(crate) use reclamation::HandleCallGuard;
pub(crate) use refinement_hooks::HandleRefinementHooks;
pub(crate) use refinement_wire::TokenWire;
#[cfg(test)]
pub(crate) use registry::{BindingState, HandleRegistryPhase, ObjectKey};
pub(crate) use registry::{
    ErasedObject, HandleRegistry, HandleRegistrySealed, LiveObjectRef, ObjectLocator,
    PendingHandleValue,
};
pub(crate) use runtime::{
    HandleRuntime, HandleRuntimeResolver, HandleRuntimeSealed, HandleRuntimeSlot, HandlesQuiescent,
};
pub(crate) use token::{HandleId, HandleToken, ObjectId, TokenCodec};
pub(crate) use topic::{
    Initialization, PrepareDecision, PublishedTopic, PublishedTopicState, TopicRemoval, TopicTable,
};
pub use typed::{ExcelHandleObject, Handle, HandleAlias, PinnedHandle};

#[cfg(test)]
mod refinement_tests;
#[cfg(test)]
mod tests;
