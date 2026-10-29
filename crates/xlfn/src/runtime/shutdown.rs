//! Runtime-wide shutdown ownership and teardown transactions.
//!
//! The lifecycle module owns only canonical protocol state and the affine
//! `RemovalClaim` issued by that state. This module owns the runtime-wide
//! removal transaction, quiescence proof, and terminal certificate logic.

mod certificate;
mod owner;
mod pipeline;

pub(crate) use certificate::{
    CanonicalCompletion, CertifiedRetirement, ClosedWitness, FinalRemoval, OpenRollback,
};
pub(crate) use owner::{RemovalClaimGuard, RemovalOwner};
#[cfg(any(test, feature = "refinement"))]
pub(crate) use pipeline::QuiescenceProof;
pub(crate) use pipeline::{CompletedResources, ReclaimFailure, ServiceSealFailure};
