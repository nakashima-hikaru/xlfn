//! Private ownership components for [`crate::runtime::Runtime`].
//!
//! Each submodule owns one protocol boundary of the runtime composition.
//! The re-exports below are the crate-internal vocabulary used by the
//! composition root; they are not a second public API.

mod formal;
mod host_ledger;
mod quarantine;
mod residency;
mod return_protocol;
mod services;

#[cfg(any(test, feature = "refinement"))]
pub(crate) use formal::FormalState;
pub(crate) use host_ledger::HostLedger;
pub(crate) use quarantine::{QuarantineReason, QuarantineVault};
pub(crate) use residency::ModuleResidency;
pub(crate) use return_protocol::ReturnProtocol;
#[cfg(feature = "async")]
pub(crate) use services::RuntimeExecutors;
pub(crate) use services::{
    GenerationServiceInputs, GenerationServices, SealedGenerationServices,
};
