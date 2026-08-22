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

#[cfg(any(test, feature = "shutdown-refinement"))]
pub(crate) use formal::FormalState;
pub(crate) use host_ledger::HostLedger;
pub(crate) use lifecycle_state::{LifecycleControl, LifecycleState, LifecycleStateKind};
pub(crate) use quarantine::{QuarantineReason, QuarantineVault};
pub(crate) use residency::ModuleResidency;
pub(crate) use return_protocol::ReturnProtocol;
pub(crate) use service_slot::{GenerationServiceRead, GenerationServiceSlot};
pub(crate) use services::RuntimeServices;
