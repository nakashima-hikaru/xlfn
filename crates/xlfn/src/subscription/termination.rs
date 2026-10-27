//! Unique termination ownership and terminal-result publication.

use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};

enum TerminationState {
    Open,
    Terminating,
    Terminated(XllResult<()>),
    Failed(XllError),
}

pub(super) struct TerminationCoordinator {
    state: Mutex<TerminationState>,
    completed: Condvar,
}

impl Default for TerminationCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(TerminationState::Open),
            completed: Condvar::new(),
        }
    }
}

impl TerminationCoordinator {
    /// Claims the sole completion authority before the caller begins cleanup.
    /// No coordinator lock survives this call into callbacks or waiting work.
    pub(super) fn begin(&self) -> TerminationClaim<'_> {
        let mut state = self.state.lock();
        match &*state {
            TerminationState::Open => {
                *state = TerminationState::Terminating;
                TerminationClaim::Owner(TerminationOwner {
                    coordinator: Some(self),
                })
            }
            TerminationState::Terminating => {
                TerminationClaim::Waiter(TerminationWaiter { coordinator: self })
            }
            TerminationState::Terminated(result) => {
                TerminationClaim::Complete(TerminationCompleted {
                    result: result.clone(),
                })
            }
            TerminationState::Failed(error) => TerminationClaim::Complete(TerminationCompleted {
                result: Err(error.clone()),
            }),
        }
    }

    fn publish(&self, result: TerminationState) {
        let mut state = self.state.lock();
        if !matches!(*state, TerminationState::Terminating) {
            xlfn_kernel::invariant::fail_stop();
        }
        *state = result;
        self.completed.notify_all();
    }

    #[cfg(test)]
    pub(super) fn phase(&self) -> TerminationPhase {
        match *self.state.lock() {
            TerminationState::Open => TerminationPhase::Open,
            TerminationState::Terminating => TerminationPhase::Terminating,
            TerminationState::Terminated(_) => TerminationPhase::Terminated,
            TerminationState::Failed(_) => TerminationPhase::Failed,
        }
    }
}

pub(super) enum TerminationClaim<'a> {
    Owner(TerminationOwner<'a>),
    Waiter(TerminationWaiter<'a>),
    Complete(TerminationCompleted),
}

/// Affine completion authority. Abandonment publishes failure and wakes waiters;
/// it never certifies that the owner's remaining cleanup has completed.
#[must_use = "complete termination, report failure, or leave a failed coordinator on Drop"]
pub(super) struct TerminationOwner<'a> {
    coordinator: Option<&'a TerminationCoordinator>,
}

impl TerminationOwner<'_> {
    /// Cleanup finished, even if one of its callbacks reported an error.
    pub(super) fn complete(mut self, result: XllResult<()>) -> XllResult<()> {
        self.coordinator
            .expect("live termination owner")
            .publish(TerminationState::Terminated(result.clone()));
        self.coordinator = None;
        result
    }

    /// Cleanup could not finish; callers must retain the affected resources.
    pub(super) fn fail(mut self, error: XllError) -> XllResult<()> {
        self.coordinator
            .expect("live termination owner")
            .publish(TerminationState::Failed(error.clone()));
        self.coordinator = None;
        Err(error)
    }
}

impl Drop for TerminationOwner<'_> {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.take() {
            coordinator.publish(TerminationState::Failed(XllError::Panic));
        }
    }
}

pub(super) struct TerminationWaiter<'a> {
    coordinator: &'a TerminationCoordinator,
}

impl TerminationWaiter<'_> {
    pub(super) fn wait(self) -> XllResult<()> {
        let mut state = self.coordinator.state.lock();
        loop {
            match &*state {
                TerminationState::Terminating => self.coordinator.completed.wait(&mut state),
                TerminationState::Terminated(result) => return result.clone(),
                TerminationState::Failed(error) => return Err(error.clone()),
                TerminationState::Open => xlfn_kernel::invariant::fail_stop(),
            }
        }
    }
}

pub(super) struct TerminationCompleted {
    result: XllResult<()>,
}

impl TerminationCompleted {
    pub(super) fn result(self) -> XllResult<()> {
        self.result
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminationPhase {
    Open,
    Terminating,
    Terminated,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(coordinator: &TerminationCoordinator) -> TerminationOwner<'_> {
        match coordinator.begin() {
            TerminationClaim::Owner(owner) => owner,
            _ => panic!("expected unique termination owner"),
        }
    }

    fn waiter(coordinator: &TerminationCoordinator) -> TerminationWaiter<'_> {
        match coordinator.begin() {
            TerminationClaim::Waiter(waiter) => waiter,
            _ => panic!("expected termination waiter"),
        }
    }

    fn completed(coordinator: &TerminationCoordinator) -> XllResult<()> {
        match coordinator.begin() {
            TerminationClaim::Complete(completed) => completed.result(),
            _ => panic!("expected terminal admission"),
        }
    }

    #[test]
    fn miri_termination_owner_publishes_same_error_to_every_waiter() {
        let coordinator = TerminationCoordinator::default();
        assert_eq!(coordinator.phase(), TerminationPhase::Open);
        let owner = owner(&coordinator);
        assert_eq!(coordinator.phase(), TerminationPhase::Terminating);
        let first = waiter(&coordinator);
        let second = waiter(&coordinator);
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let waiters = [first, second].map(|waiter| {
                let waiting_tx = waiting_tx.clone();
                let result_tx = result_tx.clone();
                scope.spawn(move || {
                    waiting_tx.send(()).unwrap();
                    result_tx.send(waiter.wait()).unwrap();
                })
            });
            for _ in 0..2 {
                waiting_rx.recv().unwrap();
            }
            assert!(matches!(
                result_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            assert!(matches!(
                owner.complete(Err(XllError::Closing)),
                Err(XllError::Closing)
            ));
            for _ in 0..2 {
                assert!(matches!(result_rx.recv().unwrap(), Err(XllError::Closing)));
            }
            for waiter in waiters {
                waiter.join().unwrap();
            }
        });
        assert_eq!(coordinator.phase(), TerminationPhase::Terminated);
        assert!(matches!(completed(&coordinator), Err(XllError::Closing)));
        assert!(matches!(completed(&coordinator), Err(XllError::Closing)));
    }

    #[test]
    fn miri_abandoned_termination_owner_wakes_waiter_with_failure() {
        let coordinator = TerminationCoordinator::default();
        let owner = owner(&coordinator);
        let waiter = waiter(&coordinator);
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let waiting = scope.spawn(move || {
                waiting_tx.send(()).unwrap();
                result_tx.send(waiter.wait()).unwrap();
            });
            waiting_rx.recv().unwrap();
            assert!(matches!(
                result_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            drop(owner);
            assert!(matches!(result_rx.recv().unwrap(), Err(XllError::Panic)));
            waiting.join().unwrap();
        });
        assert_eq!(coordinator.phase(), TerminationPhase::Failed);
        assert!(matches!(completed(&coordinator), Err(XllError::Panic)));
    }

    #[test]
    fn explicit_termination_failure_is_terminal_for_owner_and_waiters() {
        let coordinator = TerminationCoordinator::default();
        let owner = owner(&coordinator);
        let waiter = waiter(&coordinator);
        assert!(matches!(
            owner.fail(XllError::Overloaded),
            Err(XllError::Overloaded)
        ));
        assert!(matches!(waiter.wait(), Err(XllError::Overloaded)));
        assert_eq!(coordinator.phase(), TerminationPhase::Failed);
        assert!(matches!(completed(&coordinator), Err(XllError::Overloaded)));
    }

    #[test]
    fn successful_termination_cannot_be_claimed_again() {
        let coordinator = TerminationCoordinator::default();
        owner(&coordinator).complete(Ok(())).unwrap();
        assert_eq!(coordinator.phase(), TerminationPhase::Terminated);
        completed(&coordinator).unwrap();
        completed(&coordinator).unwrap();
    }
}
