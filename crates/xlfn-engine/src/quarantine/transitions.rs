//! Runtime transitions shared with the Verus quarantine proof.

/// The running ticket and next identity change only together, on acceptance.
macro_rules! claim_cleanup {
    ($progress:expr, $next:expr, $issuer:expr, $epoch:expr) => {{
        if matches!($progress, Progress::Idle | Progress::Pending) {
            let incarnation = $next;
            if let Some(next) = incarnation.checked_add(1u64) {
                $next = next;
                $progress = Progress::Running {
                    issuer: $issuer,
                    epoch: $epoch,
                    incarnation,
                };
                Some(CleanupClaim {
                    issuer: $issuer,
                    epoch: $epoch,
                    incarnation,
                })
            } else {
                None
            }
        } else {
            None
        }
    }};
}

/// Check the running stamp before consulting the caller's live epoch predicate.
macro_rules! complete_cleanup {
    ($progress:expr, $issuer:expr, $claim:expr, $epoch:ident => $current:expr, $outcome:expr) => {{
        match $progress {
            Progress::Running {
                issuer: running_issuer,
                epoch: $epoch,
                incarnation,
            } if $issuer == running_issuer
                && $claim.issuer == running_issuer
                && $claim.epoch == $epoch
                && $claim.incarnation == incarnation
                && $current =>
            {
                $progress = match $outcome {
                    CleanupOutcome::Drained => Progress::Drained($epoch),
                    CleanupOutcome::Retained => Progress::Retained($epoch),
                };
                true
            }
            _ => false,
        }
    }};
}

pub(crate) use {claim_cleanup, complete_cleanup};
