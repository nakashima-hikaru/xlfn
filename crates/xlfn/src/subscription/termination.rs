//! Unique termination ownership and terminal-result publication.
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};

/// The single initial transition permission, moved out rather than recreated
/// from a status observation. It cannot be cloned, reset or independently issued.
struct UnclaimedTermination;

enum TerminalResult {
    Completed(XllResult<()>),
    Failed(XllError),
}
impl TerminalResult {
    fn result(&self) -> XllResult<()> {
        match self {
            Self::Completed(result) => result.clone(),
            Self::Failed(error) => Err(error.clone()),
        }
    }
}
enum CompletionState {
    Pending,
    Done(TerminalResult),
}
struct TerminationState {
    unclaimed: Option<UnclaimedTermination>,
    completion: CompletionState,
}

pub(super) struct TerminationCoordinator {
    state: Mutex<TerminationState>,
    completed: Condvar,
}
impl Default for TerminationCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(TerminationState {
                unclaimed: Some(UnclaimedTermination),
                completion: CompletionState::Pending,
            }),
            completed: Condvar::new(),
        }
    }
}
impl TerminationCoordinator {
    pub(super) fn begin(&self) -> TerminationClaim<'_> {
        let mut state = self.state.lock();
        if let Some(start) = state.unclaimed.take() {
            return TerminationClaim::Owner(TerminationOwner {
                permit: Some(TerminationPermit {
                    coordinator: self,
                    _start: start,
                }),
            });
        }
        match &state.completion {
            CompletionState::Pending => {
                TerminationClaim::Waiter(TerminationWaiter { coordinator: self })
            }
            CompletionState::Done(result) => TerminationClaim::Complete(TerminationCompleted {
                result: result.result(),
            }),
        }
    }
    #[cfg(test)]
    pub(super) fn phase(&self) -> TerminationPhase {
        let state = self.state.lock();
        if state.unclaimed.is_some() {
            return TerminationPhase::Open;
        }
        match state.completion {
            CompletionState::Pending => TerminationPhase::Terminating,
            CompletionState::Done(TerminalResult::Completed(_)) => TerminationPhase::Terminated,
            CompletionState::Done(TerminalResult::Failed(_)) => TerminationPhase::Failed,
        }
    }
}

pub(super) enum TerminationClaim<'a> {
    Owner(TerminationOwner<'a>),
    Waiter(TerminationWaiter<'a>),
    Complete(TerminationCompleted),
}

struct TerminationPermit<'a> {
    coordinator: &'a TerminationCoordinator,
    _start: UnclaimedTermination,
}
impl TerminationPermit<'_> {
    fn publish(self, result: TerminalResult) {
        // Only the one initial permission can publish, and the coordinator has
        // no reset/rewind API. A broad phase check supplies no additional fact.
        self.coordinator.state.lock().completion = CompletionState::Done(result);
        self.coordinator.completed.notify_all();
    }
}

#[must_use = "complete termination, report failure, or leave failed state on Drop"]
pub(super) struct TerminationOwner<'a> {
    permit: Option<TerminationPermit<'a>>,
}
impl TerminationOwner<'_> {
    pub(super) fn complete(mut self, result: XllResult<()>) -> XllResult<()> {
        let terminal = TerminalResult::Completed(result.clone());
        self.permit
            .take()
            .expect("live termination owner")
            .publish(terminal);
        result
    }
    pub(super) fn fail(mut self, error: XllError) -> XllError {
        let terminal = TerminalResult::Failed(error.clone());
        self.permit
            .take()
            .expect("live termination owner")
            .publish(terminal);
        error
    }
}
impl Drop for TerminationOwner<'_> {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            permit.publish(TerminalResult::Failed(XllError::Panic));
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
            match &state.completion {
                CompletionState::Pending => self.coordinator.completed.wait(&mut state),
                CompletionState::Done(result) => return result.result(),
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
            XllError::Overloaded
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
