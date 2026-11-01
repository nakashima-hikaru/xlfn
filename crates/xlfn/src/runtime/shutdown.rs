//! Runtime-wide shutdown ownership and teardown transactions.
//!
//! The lifecycle module owns only canonical protocol state and the affine
//! `RemovalClaim` issued by that state. This module owns the runtime-wide
//! removal transaction, quiescence proof, and terminal certificate logic.

mod certificate;
mod finalization;
pub(crate) use finalization::FinalizedRemoval;
mod owner;
mod pipeline;

pub(crate) use certificate::{
    CanonicalCompletion, CertifiedRetirement, FinalRemoval, OpenRollback,
};
#[cfg(test)]
pub(crate) use owner::RemovalClaimGuard;
pub(crate) use owner::RemovalOwner;
#[cfg(any(test, feature = "refinement"))]
pub(crate) use pipeline::QuiescenceProof;
pub(crate) use pipeline::{CompletedResources, ReclaimFailure, ServiceSealFailure};

// The types are visible to consumers; issuance remains inside the pipeline.
#[cfg(not(feature = "async"))]
pub(crate) use pipeline::AsyncStopped;
pub(crate) use pipeline::{AddinQuiescence, GenerationCallbacksDetached, GenerationReclaimed};
