//! Opaque RTD entry transition over scalar incarnations and generations.

use vstd::prelude::*;

#[path = "../../../../crates/xlfn-services/src/rtd/transitions.rs"]
mod transitions;
use transitions::{finish_preparation, revoke_connection, revoke_server};

verus! {

#[derive(Clone, Copy)]
pub enum PendingCommitment {
    Uncommitted,
    Committed,
}

pub struct Incarnation {
    pub runtime: u64,
    pub id: u64,
}

#[allow(inconsistent_fields)]
pub enum SubscriptionPhase {
    Pending { reservations: u64, server: Option<u64> },
    AwaitConnect { reservations: u64, server: Option<u64> },
    Connecting { reservations: u64, server: u64, connection: u64, commitment: PendingCommitment },
    Active { reservations: u64, server: u64, connection: u64 },
    Revoked { reservations: u64 },
}

pub open spec fn revoked(phase: SubscriptionPhase) -> bool {
    phase is Revoked
}

pub open spec fn established_commitment(phase: SubscriptionPhase) -> bool {
    match phase {
        SubscriptionPhase::AwaitConnect { .. } => true,
        SubscriptionPhase::Connecting { commitment, .. } => commitment is Committed,
        _ => false,
    }
}

pub fn prepare_completion(
    before: SubscriptionPhase,
    entry: Incarnation,
    ticket: Incarnation,
    committed: bool,
) -> (result: (SubscriptionPhase, bool))
    ensures
        result.1 == (entry.runtime == ticket.runtime && entry.id == ticket.id),
        !result.1 ==> result.0 == before,
        revoked(before) ==> revoked(result.0) && !established_commitment(result.0),
{
    // The proof abstracts reservation accounting; only the calling Rust
    // catalog owns reservation debt and retains/reclaims its quota.
    let next = finish_preparation!(before, entry, ticket, committed, count => count);
    match next {
        Some(phase) => (phase, true),
        None => (before, false),
    }
}

pub fn terminate_server(before: SubscriptionPhase, server: u64) -> (result: (SubscriptionPhase, bool))
    ensures
        result.1 ==> revoked(result.0) && !established_commitment(result.0),
        !result.1 ==> result.0 == before,
        revoked(before) ==> revoked(result.0) && !result.1,
{
    match revoke_server!(before, server) {
        Some(phase) => (phase, true),
        None => (before, false),
    }
}

pub fn disconnect(
    before: SubscriptionPhase,
    server: u64,
    connection: u64,
) -> (result: (SubscriptionPhase, bool))
    ensures
        result.1 ==> revoked(result.0) && !established_commitment(result.0),
        !result.1 ==> result.0 == before,
        revoked(before) ==> revoked(result.0) && !result.1,
{
    match revoke_connection!(before, server, connection) {
        Some(phase) => (phase, true),
        None => (before, false),
    }
}

} // verus!
