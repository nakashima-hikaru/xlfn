#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test modules"
    )
)]
#![cfg_attr(
    not(feature = "handles"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The handle implementation is private in core-only builds"
    )
)]

mod binding;
mod connection;
mod formula;
mod lifetime;
#[allow(unsafe_code, reason = "Stable typed object pointers are audited here")]
mod object;
mod prepare;
mod publication;
#[cfg(any(test, feature = "refinement"))]
mod refinement;
mod refinement_hooks;
mod refinement_wire;
mod registry;
mod runtime;
mod store;
mod token;
mod topic;
#[allow(
    unsafe_code,
    reason = "Handle lease Send/Sync and pointer projection are audited here"
)]
mod typed;

#[cfg(test)]
use crate::call_return::ReturnContext;
#[cfg(test)]
use crate::error::DomainErrorCode;
#[cfg(test)]
pub(crate) use crate::generation::{BindingGeneration, TopicGeneration};
#[cfg(test)]
use crate::return_abi::ExcelCallbackStatus;
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
pub(crate) use connection::{FormulaBinding, FormulaObserverId, Topic};
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::FormulaCaller;
#[cfg(any(test, feature = "refinement", feature = "bench-internals"))]
pub(crate) use formula::FormulaRevisionKey;
pub(crate) use formula::HandleTopicKey;
#[cfg(feature = "handles")]
pub(crate) use formula::formula_revision_key;
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use formula::resolve_formula_caller;
#[cfg(test)]
pub(crate) use formula::test_topic_key;
#[cfg(any(
    feature = "handles",
    all(target_os = "windows", feature = "rtd"),
))]
pub(crate) use lifetime::FormulaLifetimeBackend;
#[cfg(all(
    target_os = "windows",
    any(feature = "rtd", feature = "handles"),
))]
pub(crate) use lifetime::FormulaLifetimeConnection;
pub(crate) use lifetime::FormulaLifetimeGeneration;
pub(crate) use object::SharedObject;
pub(crate) use prepare::HandlePrepareState;
pub(crate) use refinement_hooks::HandleRefinementHooks;
pub(crate) use refinement_wire::TokenWire;
#[cfg(test)]
pub(crate) use registry::HandleRegistryPhase;
#[cfg(test)]
pub(crate) use registry::{HandleRegistry, PendingHandleValue};
#[cfg(any(test, feature = "bench-internals", target_os = "windows"))]
pub(crate) use runtime::FormulaHandleService;
#[cfg(feature = "handles")]
pub(crate) use runtime::FormulaHandleServiceResolver;
#[cfg(feature = "handles")]
pub(crate) use runtime::FormulaHandleServiceSlot;
pub(crate) use store::HandleStore;
pub(crate) use token::{HandleId, HandleToken, ObjectId};
pub(crate) use topic::{
    Initialization, PrepareDecision, PublishedTopic, PublishedTopicState, TopicRemoval, TopicTable,
};
#[cfg(not(feature = "handles"))]
pub(crate) use typed::{ExcelHandleObject, Handle, HandleAlias};
#[cfg(feature = "handles")]
pub use typed::{ExcelHandleObject, Handle, HandleAlias, HandleLease, HandleObjectId};

#[cfg(all(test, feature = "handles"))]
mod refinement_tests;
#[cfg(all(test, feature = "handles"))]
#[allow(
    unsafe_code,
    reason = "Unsafe handle fixtures exercise the audited pointer boundary"
)]
mod tests;
