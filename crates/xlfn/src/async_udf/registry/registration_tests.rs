use super::*;
use crate::cancellation::CancellationGuarantee;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
const TEST_CALCULATION: CalculationEpoch = CalculationEpoch::INITIAL;

#[test]
fn miri_calculation_close_waits_for_admitted_insert_and_preserves_pins() {
    let owner = PublishedOwner::new(CalculationState::new(CalculationEpoch::INITIAL));
    // SAFETY: this private owner remains alive until after the pin drops.
    let pin = unsafe { CalculationPin::acquire(&owner) };
    let admission = owner.enter_admission().unwrap();
    let closing = owner.close_admission();
    assert!(owner.enter_admission().is_err());
    let (cancellation, token) = crate::cancellation::CancellationSource::new(
        crate::cancellation::CancellationGuarantee::BestEffort,
    );
    let (waiting_tx, waiting_rx) = std::sync::mpsc::sync_channel(1);
    let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(move || {
            waiting_tx.send(()).unwrap();
            assert!(drained_tx.send(closing.wait().drain_tasks()).is_ok());
        });
        waiting_rx.recv().unwrap();
        assert!(matches!(
            drained_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        // An admission acquired before sealing can still publish its task.
        admission.insert_task(
            TaskRegistrationId(1),
            TaskControl {
                cancellation: cancellation.into_task().0,
            },
        );
        drop(admission);
        let controls = drained_rx.recv().unwrap();
        assert_eq!(controls.len(), 1);
        assert!(owner.tasks_are_empty());
        assert!(!token.is_cancelled());
        assert_eq!(owner.pin_count(), 1);
        drop(controls);
        assert!(token.is_cancelled());
        assert_eq!(owner.pin_count(), 1);
        waiter.join().unwrap();
    });
    drop(pin);
    assert_eq!(owner.pin_count(), 0);
}

#[test]
fn test_calculation_state_sharded_removal_and_drain() {
    let state = CalculationState::new(TEST_CALCULATION);
    let admission = state.enter_admission().unwrap();
    for id in 1..=100 {
        let (cancellation, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
        admission.insert_task(
            TaskRegistrationId(id),
            TaskControl {
                cancellation: cancellation.into_task().0,
            },
        );
    }
    drop(admission);

    // Remove 40 tasks via remove_task
    for id in 1..=40 {
        assert!(state.remove_task(id));
    }
    // Drain remaining 60 tasks
    let drained = state.close_admission().wait().drain_tasks();
    assert_eq!(drained.len(), 60);
    assert!(state.tasks_are_empty());
}

#[test]
fn drained_batches_retain_cancellation_until_consumed_outside_locks() {
    for count in [0, 1, 4, 5, 33] {
        let state = CalculationState::new(TEST_CALCULATION);
        let mut observers = Vec::new();
        let admission = state.enter_admission().unwrap();
        for id in 0..count {
            let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
            observers.push(token);
            admission.insert_task(
                TaskRegistrationId(id),
                TaskControl {
                    cancellation: cancellation.into_task().0,
                },
            );
        }
        drop(admission);

        let controls = state.close_admission().wait().drain_tasks();
        assert_eq!(controls.len(), count as usize);
        assert!(state.task_shards_are_unlocked_and_empty());
        assert!(observers.iter().all(|token| !token.is_cancelled()));

        cancel_tasks(controls);
        assert!(observers.iter().all(|token| token.is_cancelled()));
    }
}

#[test]
fn calculation_drain_racing_completions_claims_every_task_once() {
    for _ in 0..8 {
        let state = CalculationState::new(TEST_CALCULATION);
        let admission = state.enter_admission().unwrap();
        for id in 0..128 {
            admission.insert_task(
                TaskRegistrationId(id),
                TaskControl {
                    cancellation: CancellationSource::new(CancellationGuarantee::BestEffort)
                        .0
                        .into_task()
                        .0,
                },
            );
        }
        drop(admission);
        let drained_admission = state.close_admission().wait();
        // No new controls can be inserted after admission drains. Existing
        // completions may remove them before or during the capacity scan.
        let barrier = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            let removers = (0..4)
                .map(|worker| {
                    let state = &state;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        (worker..128)
                            .step_by(4)
                            .filter(|&id| state.remove_task(id))
                            .count()
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            let drained = drained_admission.drain_tasks();
            let completed: usize = removers
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .sum();
            assert_eq!(completed + drained.len(), 128);
            assert!(state.tasks_are_empty());
        });
    }
}

#[test]
fn removing_task_releases_cancellation_wakers_outside_task_lock() {
    struct ReentrantWake(Arc<CalculationState>, Arc<AtomicBool>);
    impl futures_util::task::ArcWake for ReentrantWake {
        fn wake_by_ref(this: &Arc<Self>) {
            let unlocked = this.0.task_shard_is_unlocked(0);
            this.1.store(unlocked, Ordering::Release);
            if unlocked {
                assert!(!this.0.remove_task(0));
            }
        }
    }
    let generation = Arc::new(CalculationState::new(TEST_CALCULATION));
    let observed = Arc::new(AtomicBool::new(false));
    let waker = futures_util::task::waker(Arc::new(ReentrantWake(
        Arc::clone(&generation),
        Arc::clone(&observed),
    )));
    let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    let mut waiter = std::pin::pin!(token.cancelled());
    assert!(
        waiter
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker))
            .is_pending()
    );
    generation.enter_admission().unwrap().insert_task(
        TaskRegistrationId(0),
        TaskControl {
            cancellation: source.into_task().0,
        },
    );
    assert!(generation.remove_task(0));
    assert!(observed.load(Ordering::Acquire));
    assert!(generation.tasks_are_empty());
}
