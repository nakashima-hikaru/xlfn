//! Single owning state and writer boundary for the add-in lifecycle.

use crate::generation::{OpenAttemptId, RemovalEpoch};
use crate::lifecycle_sequence::{LifecycleSequence, OpenClaimError};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::ops::Deref;

/// All canonical phases. Payloads remain opaque to the engine and need not
/// implement Clone, Copy, or any host-specific trait.
pub enum LifecycleState<Opening, Open, Stopping, Quarantined> {
    Closed,
    Opening {
        attempt: OpenAttemptId,
        payload: Opening,
    },
    Open {
        bundle: Open,
    },
    Stopping(Stopping),
    Quarantined(Quarantined),
}

impl<Opening, Open, Stopping, Quarantined> LifecycleState<Opening, Open, Stopping, Quarantined> {
    /// Claim an open attempt and install its initial native payload as one
    /// decision. The caller supplies both values under the canonical lock.
    pub fn begin_open(
        &mut self,
        sequence: &mut LifecycleSequence,
        sampled_epoch: RemovalEpoch,
        payload: Opening,
    ) -> Result<OpenAttemptId, OpenClaimError> {
        let attempt = sequence.claim_open(matches!(self, Self::Closed), sampled_epoch)?;
        *self = Self::Opening { attempt, payload };
        Ok(attempt)
    }

    /// Commit only the requested opening. If its opaque payload is not yet
    /// publishable the classifier returns that same owner, restoring Opening.
    /// This classifier is part of the locked transition and must not unwind.
    pub fn commit_open<Effect>(
        &mut self,
        expected: OpenAttemptId,
        classify: impl FnOnce(Opening) -> Result<(Open, Effect), Opening>,
    ) -> Option<Effect> {
        let (attempt, payload) = match std::mem::replace(self, Self::Closed) {
            Self::Opening { attempt, payload } if attempt == expected => (attempt, payload),
            other => {
                *self = other;
                return None;
            }
        };
        let fail_closed = std::mem::DropGuard::new((), |()| std::process::abort());
        let outcome = match classify(payload) {
            Ok((bundle, effect)) => {
                *self = Self::Open { bundle };
                Some(effect)
            }
            Err(payload) => {
                *self = Self::Opening { attempt, payload };
                None
            }
        };
        std::mem::DropGuard::dismiss(fail_closed);
        outcome
    }
}

/// A stop is either driven by rollback/removal or awaiting boundary evidence.
pub enum StoppingState<Remove, RollbackOpen, Finalizing> {
    Remove(Remove),
    RollbackOpen(RollbackOpen),
    Finalizing(Finalizing),
}

/// Non-poisoning ownership lock with one condition variable for this core.
/// External waiting or native effects that can reenter must run outside it.
pub struct Canonical<Core> {
    core: Mutex<Core>,
    changed: Condvar,
}

impl<Core> Canonical<Core> {
    pub const fn new(core: Core) -> Self {
        Self {
            core: Mutex::new(core),
            changed: Condvar::new(),
        }
    }

    pub fn access(&self) -> Access<'_, Core> {
        Access {
            issuer: self,
            core: self.core.lock(),
        }
    }

    pub fn try_access(&self) -> Option<Access<'_, Core>> {
        Some(Access {
            issuer: self,
            core: self.core.try_lock()?,
        })
    }

    pub fn notify_all(&self) {
        self.changed.notify_all();
    }
}

/// The owning lock guard is read-only outside an explicit transaction.
/// Waiting is tied to the issuer's own condition variable.
pub struct Access<'owner, Core> {
    issuer: &'owner Canonical<Core>,
    core: MutexGuard<'owner, Core>,
}

impl<Core> Access<'_, Core> {
    /// Core writes and the corresponding read-side projection occur under the
    /// same lock. Existing native owners handle aborted mutations; neither
    /// closure may call user code or acquire this lock recursively.
    pub fn transition<R, Effect>(
        &mut self,
        mutation: impl FnOnce(&mut Core) -> (R, Effect),
        project: impl FnOnce(&Core, Effect),
    ) -> R {
        let (result, effect) = mutation(&mut self.core);
        project(&self.core, effect);
        self.issuer.notify_all();
        result
    }

    /// Wait for a possible state change; callers must recheck the condition.
    pub fn wait(&mut self) {
        self.issuer.changed.wait(&mut self.core);
    }
}

impl<Core> Deref for Access<'_, Core> {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_owner_and_projection_are_committed_under_the_same_lock() {
        type State = LifecycleState<String, (), (), ()>;
        let core = Canonical::new((State::Closed, LifecycleSequence::new()));
        let attempt = core.access().transition(
            |(state, sequence)| {
                let attempt = state
                    .begin_open(
                        sequence,
                        RemovalEpoch::new(0),
                        String::from("non-copy owner"),
                    )
                    .unwrap();
                (attempt, "publish opening")
            },
            |(state, _), effect| {
                assert_eq!(effect, "publish opening");
                assert!(core.try_access().is_none());
                assert!(
                    matches!(state, State::Opening { payload, .. } if payload == "non-copy owner")
                );
            },
        );
        let access = core.access();
        assert!(matches!(&access.0, State::Opening { attempt: id, .. } if *id == attempt));
    }

    #[test]
    fn stale_open_commit_never_touches_the_native_owner() {
        type State = LifecycleState<String, String, (), ()>;
        let mut sequence = LifecycleSequence::new();
        let mut state = State::Closed;
        let attempt = state
            .begin_open(&mut sequence, RemovalEpoch::new(0), String::from("owner"))
            .unwrap();
        let stale = OpenAttemptId::new(attempt.get() + 1).unwrap();
        assert!(
            state
                .commit_open::<()>(stale, |_| panic!("do not classify stale owner"))
                .is_none()
        );
        assert!(state.commit_open::<()>(attempt, Err).is_none());
        assert!(matches!(&state, State::Opening { payload, .. } if payload == "owner"));
        assert_eq!(
            state.commit_open(attempt, |owner| Ok((owner, 42))),
            Some(42)
        );
        assert!(matches!(state, State::Open { bundle } if bundle == "owner"));
    }
}
