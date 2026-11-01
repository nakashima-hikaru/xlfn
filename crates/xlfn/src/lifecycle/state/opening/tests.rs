use super::super::{ClosedState, LifecycleCoordinator, LifecycleState};
use super::OpenStart;
use crate::XllError;
use crate::diagnostics::id::DiagnosticId;
use crate::generation::{RemovalAttemptId, RemovalEpoch};
use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
use std::sync::atomic::Ordering;

#[derive(Debug, Eq, PartialEq)]
struct Snapshot {
    canonical_phase: LifecyclePhase,
    projected_phase: LifecyclePhase,
    host_intent: HostLifecycleIntent,
    next_open_sequence: u64,
    removal_epoch: u64,
    removal_attempt: Option<RemovalAttemptId>,
    publication_empty: bool,
}

fn snapshot(coordinator: &LifecycleCoordinator<()>) -> Snapshot {
    let core = coordinator.core.lock();
    Snapshot {
        canonical_phase: core.state.phase(),
        projected_phase: LifecyclePhase::from_raw(coordinator.phase.load(Ordering::Acquire)),
        host_intent: core.host_intent,
        next_open_sequence: core.next_lifecycle_attempt,
        removal_epoch: core.removal_epoch,
        removal_attempt: core.state.removal_attempt(),
        publication_empty: coordinator.publication.load(Ordering::Acquire).is_null(),
    }
}

#[test]
fn miri_idle_start_keeps_its_lock_and_abandonment_quarantines_only_its_issuer() {
    let issuer = LifecycleCoordinator::<()>::new();
    let other = LifecycleCoordinator::<()>::new();
    let other_before = snapshot(&other);
    {
        let mut core = issuer.core.lock();
        core.host_intent = HostLifecycleIntent::ExplicitRemovalComplete;
        core.removal_epoch = 9;
        core.next_lifecycle_attempt = 41;
    }

    let start = OpenStart::acquire(&issuer, RemovalEpoch::new(9)).unwrap();
    assert_eq!(start.attempt().get(), 41);
    assert_eq!(start.access().open_attempt(), Some(start.attempt()));
    assert_eq!(start.access().next_open_sequence(), 42);
    assert_eq!(start.access().host_intent(), HostLifecycleIntent::None);
    assert_eq!(start.access().phase(), LifecyclePhase::Opening);
    assert_eq!(
        LifecyclePhase::from_raw(issuer.phase.load(Ordering::Acquire)),
        LifecyclePhase::Opening
    );
    assert!(issuer.core.try_lock().is_none());
    assert_eq!(snapshot(&other), other_before);

    drop(start);
    let after = snapshot(&issuer);
    assert_eq!(after.canonical_phase, LifecyclePhase::Quarantined);
    assert_eq!(after.projected_phase, LifecyclePhase::Quarantined);
    assert_eq!(after.next_open_sequence, 42);
    assert!(after.publication_empty);
    assert_eq!(snapshot(&other), other_before);
    assert!(OpenStart::acquire(&issuer, RemovalEpoch::new(9)).is_err());
}

#[test]
fn miri_closed_finalization_cannot_be_mistaken_for_idle_before_start() {
    let coordinator = LifecycleCoordinator::<()>::new();
    let removal = RemovalAttemptId::new(7).unwrap();
    {
        let mut core = coordinator.core.lock();
        core.state = LifecycleState::Closed(ClosedState::Finalizing { attempt: removal });
        core.host_intent = HostLifecycleIntent::ExplicitRemovalComplete;
        core.removal_epoch = 3;
        core.next_lifecycle_attempt = 17;
    }
    let before = snapshot(&coordinator);
    assert_eq!(before.canonical_phase, LifecyclePhase::Closed);
    assert_eq!(before.projected_phase, LifecyclePhase::Closed);
    assert_eq!(before.removal_attempt, Some(removal));

    let error = OpenStart::acquire(&coordinator, RemovalEpoch::new(3))
        .err()
        .expect("a finalizing removal still owns the closed lifecycle");
    assert!(matches!(
        error,
        XllError::Internal {
            diagnostic_id: DiagnosticId::OPEN_PHASE,
        }
    ));
    assert_eq!(snapshot(&coordinator), before);
}

#[test]
fn miri_stale_removal_epoch_is_rejected_without_starting_or_consuming_an_attempt() {
    let coordinator = LifecycleCoordinator::<()>::new();
    {
        let mut core = coordinator.core.lock();
        core.host_intent = HostLifecycleIntent::ExplicitRemovalComplete;
        core.removal_epoch = 12;
        core.next_lifecycle_attempt = 23;
    }
    let before = snapshot(&coordinator);
    let error = OpenStart::acquire(&coordinator, RemovalEpoch::new(11))
        .err()
        .expect("a stale epoch cannot start a new invocation");
    assert!(matches!(
        error,
        XllError::Internal {
            diagnostic_id: DiagnosticId::OPEN_PHASE,
        }
    ));
    assert_eq!(snapshot(&coordinator), before);
}

#[test]
fn miri_attempt_exhaustion_preserves_closed_intent_and_canonical_state() {
    let coordinator = LifecycleCoordinator::<()>::new();
    {
        let mut core = coordinator.core.lock();
        core.host_intent = HostLifecycleIntent::ExplicitRemovalComplete;
        core.removal_epoch = 5;
        core.next_lifecycle_attempt = u64::MAX;
    }
    let before = snapshot(&coordinator);
    let error = OpenStart::acquire(&coordinator, RemovalEpoch::new(5))
        .err()
        .expect("exhausted identities cannot start an invocation");
    assert!(matches!(
        error,
        XllError::Internal {
            diagnostic_id: DiagnosticId::ATTEMPT_OVERFLOW,
        }
    ));
    assert_eq!(snapshot(&coordinator), before);
}

#[test]
fn miri_unwinding_a_started_owner_quarantines_its_original_coordinator() {
    let issuer = LifecycleCoordinator::<()>::new();
    let other = LifecycleCoordinator::<()>::new();
    let other_before = snapshot(&other);
    let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
        let _start = OpenStart::acquire(&issuer, RemovalEpoch::new(0)).unwrap();
        panic!("after canonical opening start");
    }));
    assert!(result.is_err());
    let after = snapshot(&issuer);
    assert_eq!(after.canonical_phase, LifecyclePhase::Quarantined);
    assert_eq!(after.projected_phase, LifecyclePhase::Quarantined);
    assert!(after.publication_empty);
    assert_eq!(snapshot(&other), other_before);
}

#[test]
#[allow(
    clippy::mem_forget,
    reason = "safe forgetting must retain the original lock instead of enabling another start"
)]
fn forgotten_start_retains_its_lock_and_never_publishes_a_generation() {
    // Keep the allocation alive for the full lifetime of the forgotten guard.
    let coordinator: &'static LifecycleCoordinator<()> =
        Box::leak(Box::new(LifecycleCoordinator::new()));
    let start = OpenStart::acquire(coordinator, RemovalEpoch::new(0)).unwrap();
    assert_eq!(start.access().phase(), LifecyclePhase::Opening);
    std::mem::forget(start);

    assert_eq!(
        LifecyclePhase::from_raw(coordinator.phase.load(Ordering::Acquire)),
        LifecyclePhase::Opening
    );
    assert!(coordinator.core.try_lock().is_none());
    assert!(coordinator.publication.load(Ordering::Acquire).is_null());
}
