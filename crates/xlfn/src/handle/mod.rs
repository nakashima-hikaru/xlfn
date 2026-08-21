pub(crate) use crate::generation::{BindingGeneration, ObjectGeneration, TopicGeneration};
use crate::{DomainErrorCode, ExcelCallbackStatus, ReturnContext, XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::any::{TypeId, type_name};
use std::cell::Cell;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

mod connection;
mod formula;
mod prepare;
mod reclamation;
#[cfg(any(test, feature = "handle-refinement-trace"))]
mod refinement;
#[cfg(any(test, feature = "handle-refinement-trace"))]
mod refinement_hooks;
mod registry;
mod runtime;
mod token;
mod topic;
mod typed;

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
pub(crate) use reclamation::{HandleCallGuard, TypedObjectRef};
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) use refinement::TokenWire;
#[cfg(any(test, feature = "handle-refinement-trace"))]
pub(crate) use refinement_hooks::HandleRefinementHooks;
#[cfg(test)]
pub(crate) use registry::{BindingState, HandleRegistryPhase, ObjectKey};
pub(crate) use registry::{
    ErasedObject, HandleRegistry, HandleRegistrySealed, ObjectLocator, ObjectPin,
    PendingHandleValue,
};
pub(crate) use runtime::{
    HandleRuntime, HandleRuntimeResolver, HandleRuntimeSealed, HandleRuntimeSlot, HandlesQuiescent,
};
pub(crate) use token::{HandleId, HandleToken, ObjectId, TokenCodec};
pub(crate) use topic::{
    Initialization, PrepareDecision, PublishedTopic, PublishedTopicState, TopicTable,
};
pub use typed::{AsyncHandle, ExcelHandleObject, Handle, HandleAlias, PinnedHandle};

#[cfg(test)]
mod refinement_tests;
#[cfg(test)]
mod tests;
