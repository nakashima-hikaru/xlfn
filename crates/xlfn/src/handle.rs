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
mod domain;
mod formula;
mod lifetime;
#[allow(unsafe_code, reason = "Stable typed object pointers are audited here")]
mod object;
mod prepare;
#[cfg(feature = "bench-internals")]
mod protocol_bench;
mod publication;
#[cfg(feature = "bench-internals")]
pub use protocol_bench::{handle_removal_probe, handle_retirement_debt_probe};
#[cfg(any(test, feature = "refinement"))]
mod refinement;
mod refinement_hooks;
mod refinement_wire;
#[allow(
    unsafe_code,
    reason = "Registry object and domain capability management"
)]
mod registry;
mod runtime;
mod store;
mod token;
#[cfg(feature = "bench-internals")]
mod token_cache_bench;
#[cfg(feature = "bench-internals")]
pub use token_cache_bench::token_cache_associativity_probe;
mod topic;
#[allow(
    unsafe_code,
    reason = "Handle lease Send/Sync and pointer projection are audited here"
)]
mod typed;

pub(crate) use crate::call::HandleDomainWitness;
#[cfg(any(target_os = "windows", test))]
pub(crate) use connection::HandleConnection;
pub(crate) use connection::{FormulaObserverId, Topic};
pub(crate) use domain::{HandleDomainPermit, HandleReadDomain};
#[cfg(any(feature = "bench-internals", all(test, feature = "handles")))]
pub(crate) use formula::FormulaCaller;
#[cfg(any(test, feature = "refinement", feature = "bench-internals"))]
pub(crate) use formula::FormulaRevisionKey;
pub(crate) use formula::HandleTopicKey;
#[cfg(feature = "handles")]
pub(crate) use formula::formula_revision_key;
#[cfg(feature = "bench-internals")]
pub(crate) use formula::resolve_formula_caller;
#[cfg(all(test, feature = "handles"))]
pub(crate) use formula::test_topic_key;
#[cfg(any(feature = "handles", all(target_os = "windows", feature = "rtd"),))]
pub(crate) use lifetime::FormulaLifetimeBackend;
#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles"),))]
pub(crate) use lifetime::FormulaLifetimeConnection;
pub(crate) use lifetime::FormulaLifetimeGeneration;
pub(crate) use prepare::HandlePrepareState;
pub(crate) use refinement_hooks::HandleRefinementHooks;
pub(crate) use refinement_wire::TokenWire;
#[cfg(feature = "handles")]
pub(crate) use registry::HandleRegistry;
#[cfg(any(test, target_os = "windows", feature = "bench-internals"))]
pub(crate) use runtime::FormulaHandleService;
#[cfg(all(feature = "handles", any(test, feature = "bench-internals")))]
pub(crate) use runtime::FormulaHandleServiceRead;
#[cfg(feature = "handles")]
pub(crate) use runtime::FormulaHandleServiceResolver;
#[cfg(feature = "handles")]
pub(crate) use runtime::FormulaHandleServiceSlot;
pub(crate) use store::HandleStore;
pub(crate) use token::{HandleId, HandleToken, ObjectId};
pub(crate) use topic::{
    Initialization, InitializationPtr, PrepareDecision, PublishedTopic, PublishedTopicPtr,
    PublishedTopicState, TopicRemoval, TopicTable,
};
#[cfg(all(feature = "async", feature = "handles"))]
pub(crate) use typed::GenerationLeaseBrand;
#[cfg(all(feature = "async", feature = "handles"))]
pub use typed::PendingHandleLease;
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
