//! Sequential quarantine machine over arbitrary issuer/epoch identities.

use std::num::NonZeroU64;
use vstd::prelude::*;

#[path = "../../../../crates/xlfn-engine/src/quarantine/transitions.rs"]
mod transitions;
use transitions::{claim_cleanup, complete_cleanup};

verus! {

pub enum Progress {
    Idle,
    Pending,
    Running { issuer: u64, epoch: u64, incarnation: NonZeroU64 },
    Drained(u64),
    Retained(u64),
}

pub struct CleanupClaim {
    pub issuer: u64,
    pub epoch: u64,
    pub incarnation: NonZeroU64,
}

pub enum CleanupOutcome {
    Drained,
    Retained,
}

pub open spec fn selectable(progress: Progress) -> bool {
    match progress {
        Progress::Idle | Progress::Pending => true,
        _ => false,
    }
}

pub fn verify_claim(
    before: Progress,
    next_before: NonZeroU64,
    issuer: u64,
    epoch: u64,
) -> (result: (Progress, NonZeroU64, Option<CleanupClaim>))
    ensures
        match result.2 {
            Some(claim) => selectable(before)
                && next_before.get() < u64::MAX
                && result.0 == (Progress::Running { issuer, epoch, incarnation: next_before })
                && result.1.get() as int == next_before.get() as int + 1
                && claim.issuer == issuer
                && claim.epoch == epoch
                && claim.incarnation == next_before,
            None => (!selectable(before) || next_before.get() == u64::MAX)
                && result.0 == before
                && result.1 == next_before,
        },
        !selectable(before) ==> result.2.is_none(),
        result.2.is_some() ==> !selectable(result.0),
{
    let mut progress = before;
    let mut next = next_before;
    let ticket = claim_cleanup!(progress, next, issuer, epoch);
    (progress, next, ticket)
}

pub open spec fn exact(
    before: Progress,
    caller: u64,
    claim: CleanupClaim,
    current: u64,
) -> bool {
    match before {
        Progress::Running { issuer, epoch, incarnation } =>
            caller == issuer && claim.issuer == issuer && claim.epoch == epoch
                && claim.incarnation@ == incarnation@ && current == epoch,
        _ => false,
    }
}

pub open spec fn terminal(outcome: CleanupOutcome, epoch: u64) -> Progress {
    match outcome {
        CleanupOutcome::Drained => Progress::Drained(epoch),
        CleanupOutcome::Retained => Progress::Retained(epoch),
    }
}

pub fn verify_completion(
    before: Progress,
    caller: u64,
    claim: CleanupClaim,
    current: u64,
    outcome: CleanupOutcome,
) -> (result: (Progress, bool))
    ensures
        result.1 == exact(before, caller, claim, current),
        !result.1 ==> result.0 == before,
        result.1 ==> result.0 == terminal(outcome, claim.epoch) && !selectable(result.0),
{
    let mut progress = before;
    let completed = complete_cleanup!(progress, caller, claim, epoch => current == epoch, outcome);
    (progress, completed)
}

} // verus!
