//! Private ownership components for [`crate::runtime::Runtime`].
//!
//! Each submodule owns one protocol boundary of the runtime composition.
//! The re-exports below are the crate-internal vocabulary used by the
//! composition root; they are not a second public API.

mod formal;
mod host_ledger;
mod lifecycle_state;
mod quarantine;
mod residency;
mod return_protocol;
mod service_slot;
mod services;
mod thread_affine;

#[cfg(any(test, feature = "unstable"))]
pub(crate) use formal::FormalState;
pub(crate) use host_ledger::HostLedger;
pub(crate) use lifecycle_state::{LifecycleCoordinator, LifecycleCore};
pub(crate) use quarantine::{QuarantineReason, QuarantineVault};
pub(crate) use residency::ModuleResidency;
pub(crate) use return_protocol::ReturnProtocol;
pub(crate) use service_slot::{GenerationServiceRead, GenerationServiceSlot};
pub(crate) use services::GenerationServices;
#[cfg(feature = "async")]
pub(crate) use services::RuntimeExecutors;
pub(crate) use thread_affine::{
    ThreadAffineAccess, ThreadAffineError, ThreadAffineInstallError, ThreadAffineSlot,
};
