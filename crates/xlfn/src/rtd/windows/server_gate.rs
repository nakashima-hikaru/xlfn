use super::event::{ManualResetEvent, Win32EventError};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread::ThreadId;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ServerPhase {
    #[default]
    Open,
    Terminating {
        owner: ThreadId,
        deferred: bool,
    },
    Terminated,
}

#[derive(Default)]
pub(super) struct ServerOperationState {
    pub(super) phase: ServerPhase,
    termination_coordinator: Option<ThreadId>,
    pub(super) in_flight: usize,
    notifications_in_flight: usize,
    in_flight_by_thread: HashMap<ThreadId, usize>,
    notifications_in_flight_by_thread: HashMap<ThreadId, usize>,
}

pub(super) struct ServerOperationBarrier {
    pub(super) state: Mutex<ServerOperationState>,
    quiescent: ManualResetEvent,
    termination_finished: ManualResetEvent,
}

pub(super) struct ServerOperation<'a> {
    barrier: &'a ServerOperationBarrier,
    thread_id: ThreadId,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

pub(super) struct ServerNotificationOperation<'a> {
    barrier: &'a ServerOperationBarrier,
    thread_id: ThreadId,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

pub(super) struct ServerTermination<'a> {
    barrier: &'a ServerOperationBarrier,
    owner: ThreadId,
    deferred: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

pub(super) enum ServerTerminationRequest<'a> {
    Complete,
    InProgress,
    Synchronous(ServerTermination<'a>),
    Deferred(DeferredTerminationReservation<'a>),
}

pub(super) struct DeferredTerminationReservation<'a> {
    barrier: &'a ServerOperationBarrier,
    pub(super) owner: ThreadId,
    state: MutexGuard<'a, ServerOperationState>,
    committed: bool,
}

impl ServerOperationBarrier {
    pub(super) fn new() -> Result<Self, Win32EventError> {
        Ok(Self {
            state: Mutex::new(ServerOperationState::default()),
            quiescent: ManualResetEvent::new(false)?,
            termination_finished: ManualResetEvent::new(false)?,
        })
    }

    pub(super) fn enter(&self) -> Option<ServerOperation<'_>> {
        let thread_id = std::thread::current().id();
        let mut state = self.state.lock();
        if state.phase != ServerPhase::Open {
            return None;
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .expect("RTD COM operation count cannot overflow");
        let per_thread = state.in_flight_by_thread.entry(thread_id).or_default();
        *per_thread = per_thread
            .checked_add(1)
            .expect("per-thread RTD COM operation count cannot overflow");
        Some(ServerOperation {
            barrier: self,
            thread_id,
            _not_send_or_sync: PhantomData,
        })
    }

    pub(super) fn enter_notification(&self) -> Option<ServerNotificationOperation<'_>> {
        let thread_id = std::thread::current().id();
        let mut state = self.state.lock();
        if state.phase != ServerPhase::Open {
            return None;
        }
        state.notifications_in_flight = state
            .notifications_in_flight
            .checked_add(1)
            .expect("RTD notification operation count cannot overflow");
        let per_thread = state
            .notifications_in_flight_by_thread
            .entry(thread_id)
            .or_default();
        *per_thread = per_thread
            .checked_add(1)
            .expect("per-thread RTD notification count cannot overflow");
        Some(ServerNotificationOperation {
            barrier: self,
            thread_id,
            _not_send_or_sync: PhantomData,
        })
    }

    pub(super) fn request_termination(
        &self,
    ) -> Result<ServerTerminationRequest<'_>, ServerCloseError> {
        let owner = std::thread::current().id();
        let mut state = self.state.lock();
        match state.phase {
            ServerPhase::Terminated => return Ok(ServerTerminationRequest::Complete),
            ServerPhase::Terminating { .. } => {
                return Ok(ServerTerminationRequest::InProgress);
            }
            ServerPhase::Open => {}
        }

        self.quiescent.reset().map_err(ServerCloseError::Event)?;
        self.termination_finished
            .reset()
            .map_err(ServerCloseError::Event)?;
        let deferred = state.in_flight != 0 || state.notifications_in_flight != 0;
        state.termination_coordinator = None;
        state.phase = ServerPhase::Terminating { owner, deferred };

        if deferred {
            Ok(ServerTerminationRequest::Deferred(
                DeferredTerminationReservation {
                    barrier: self,
                    owner,
                    state,
                    committed: false,
                },
            ))
        } else {
            drop(state);
            Ok(ServerTerminationRequest::Synchronous(ServerTermination {
                barrier: self,
                owner,
                deferred: false,
                _not_send_or_sync: PhantomData,
            }))
        }
    }

    pub(super) fn wait_for_deferred_termination(
        &self,
        owner: ThreadId,
    ) -> Result<ServerTermination<'_>, ServerCloseError> {
        loop {
            {
                let mut state = self.state.lock();
                match state.phase {
                    ServerPhase::Terminating {
                        owner: active_owner,
                        deferred: true,
                    } if active_owner == owner => {
                        if state.in_flight == 0 && state.notifications_in_flight == 0 {
                            state.termination_coordinator = Some(std::thread::current().id());
                            return Ok(ServerTermination {
                                barrier: self,
                                owner,
                                deferred: true,
                                _not_send_or_sync: PhantomData,
                            });
                        }
                    }
                    _ => return Err(ServerCloseError::Reentrant),
                }
            }

            self.quiescent
                .wait_blocking()
                .map_err(ServerCloseError::WaitFailed)?;
        }
    }

    pub(super) fn close_and_wait(&self) -> Result<Option<ServerTermination<'_>>, ServerCloseError> {
        self.close_and_wait_with(ManualResetEvent::wait_with_com_pumping)
    }

    pub(super) fn close_and_wait_with(
        &self,
        wait: impl Fn(&ManualResetEvent) -> Result<(), i32>,
    ) -> Result<Option<ServerTermination<'_>>, ServerCloseError> {
        let thread_id = std::thread::current().id();
        loop {
            #[derive(Clone, Copy)]
            enum WaitTarget {
                Quiescence,
                Termination,
            }

            let target = {
                let mut state = self.state.lock();
                // Check this before waiting for another termination owner: that
                // owner may itself be waiting for this thread's entered work.
                if state
                    .in_flight_by_thread
                    .get(&thread_id)
                    .is_some_and(|count| *count != 0)
                    || state
                        .notifications_in_flight_by_thread
                        .get(&thread_id)
                        .is_some_and(|count| *count != 0)
                    || state.termination_coordinator == Some(thread_id)
                {
                    return Err(ServerCloseError::Reentrant);
                }

                match state.phase {
                    ServerPhase::Terminated => return Ok(None),
                    ServerPhase::Terminating {
                        owner,
                        deferred: false,
                    } if owner == thread_id => {
                        return Err(ServerCloseError::Reentrant);
                    }
                    ServerPhase::Terminating { .. } => WaitTarget::Termination,
                    ServerPhase::Open => {
                        // Reset both events before publishing Terminating while
                        // holding the state lock. No entered operation can reach
                        // zero between the reset and phase transition.
                        self.quiescent.reset().map_err(ServerCloseError::Event)?;
                        self.termination_finished
                            .reset()
                            .map_err(ServerCloseError::Event)?;
                        state.phase = ServerPhase::Terminating {
                            owner: thread_id,
                            deferred: false,
                        };
                        state.termination_coordinator = None;

                        if state.in_flight == 0 && state.notifications_in_flight == 0 {
                            return Ok(Some(ServerTermination {
                                barrier: self,
                                owner: thread_id,
                                deferred: false,
                                _not_send_or_sync: PhantomData,
                            }));
                        }

                        WaitTarget::Quiescence
                    }
                }
            };

            let event = match target {
                WaitTarget::Quiescence => &self.quiescent,
                WaitTarget::Termination => &self.termination_finished,
            };

            if let Err(status) = wait(event) {
                if matches!(target, WaitTarget::Quiescence) {
                    let mut state = self.state.lock();
                    if state.phase
                        == (ServerPhase::Terminating {
                            owner: thread_id,
                            deferred: false,
                        })
                    {
                        // The owner has not begun teardown yet, so reopening is
                        // safe. Wake any secondary closer so it can observe the
                        // rollback instead of waiting for a termination that no
                        // longer has an owner.
                        state.phase = ServerPhase::Open;
                        state.termination_coordinator = None;
                        self.termination_finished
                            .set()
                            .unwrap_or_else(|_| std::process::abort());
                    }
                }
                return Err(ServerCloseError::WaitFailed(status));
            }

            let state = self.state.lock();
            match target {
                WaitTarget::Quiescence => {
                    if state.phase
                        == (ServerPhase::Terminating {
                            owner: thread_id,
                            deferred: false,
                        })
                        && state.in_flight == 0
                        && state.notifications_in_flight == 0
                    {
                        return Ok(Some(ServerTermination {
                            barrier: self,
                            owner: thread_id,
                            deferred: false,
                            _not_send_or_sync: PhantomData,
                        }));
                    }
                }
                WaitTarget::Termination if state.phase == ServerPhase::Terminated => {
                    return Ok(None);
                }
                WaitTarget::Termination => {}
            }
        }
    }
}

impl DeferredTerminationReservation<'_> {
    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for DeferredTerminationReservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        debug_assert_eq!(
            self.state.phase,
            ServerPhase::Terminating {
                owner: self.owner,
                deferred: true,
            }
        );
        self.state.phase = ServerPhase::Open;
        self.state.termination_coordinator = None;
        self.barrier
            .termination_finished
            .set()
            .unwrap_or_else(|_| std::process::abort());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ServerCloseError {
    Reentrant,
    Event(Win32EventError),
    WaitFailed(i32),
    WorkerPanicked,
}

#[cfg(test)]
impl Default for ServerOperationBarrier {
    fn default() -> Self {
        Self::new().expect("Windows must create RTD barrier events for tests")
    }
}

impl Drop for ServerOperation<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock();
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("RTD COM operation count remains balanced");
        let remove_thread = {
            let per_thread = state
                .in_flight_by_thread
                .get_mut(&self.thread_id)
                .expect("entered RTD COM operation has a per-thread count");
            *per_thread = per_thread
                .checked_sub(1)
                .expect("per-thread RTD COM operation count remains balanced");
            *per_thread == 0
        };
        if remove_thread {
            state.in_flight_by_thread.remove(&self.thread_id);
        }
        if state.in_flight == 0
            && state.notifications_in_flight == 0
            && matches!(state.phase, ServerPhase::Terminating { .. })
        {
            self.barrier
                .quiescent
                .set()
                .unwrap_or_else(|_| std::process::abort());
        }
    }
}

impl Drop for ServerNotificationOperation<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock();
        state.notifications_in_flight = state
            .notifications_in_flight
            .checked_sub(1)
            .expect("RTD notification operation count remains balanced");
        let remove_thread = {
            let per_thread = state
                .notifications_in_flight_by_thread
                .get_mut(&self.thread_id)
                .expect("entered RTD notification has a per-thread count");
            *per_thread = per_thread
                .checked_sub(1)
                .expect("per-thread RTD notification count remains balanced");
            *per_thread == 0
        };
        if remove_thread {
            state
                .notifications_in_flight_by_thread
                .remove(&self.thread_id);
        }
        if state.in_flight == 0
            && state.notifications_in_flight == 0
            && matches!(state.phase, ServerPhase::Terminating { .. })
        {
            self.barrier
                .quiescent
                .set()
                .unwrap_or_else(|_| std::process::abort());
        }
    }
}

impl Drop for ServerTermination<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock();
        debug_assert_eq!(
            state.phase,
            ServerPhase::Terminating {
                owner: self.owner,
                deferred: self.deferred,
            }
        );
        state.phase = ServerPhase::Terminated;
        state.termination_coordinator = None;
        self.barrier
            .termination_finished
            .set()
            .unwrap_or_else(|_| std::process::abort());
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TerminationWorkerStatus {
    #[default]
    Idle,
    Starting,
    Running,
    Joining,
    Joined,
}

#[derive(Default)]
pub(super) struct TerminationWorkerState {
    pub(super) status: TerminationWorkerStatus,
    handle: Option<std::thread::JoinHandle<()>>,
    thread_id: Option<ThreadId>,
}

#[derive(Default)]
pub(super) struct TerminationWorker {
    pub(super) state: Mutex<TerminationWorkerState>,
    changed: Condvar,
}

pub(super) struct TerminationWorkerStart<'a> {
    worker: &'a TerminationWorker,
    committed: bool,
}

impl TerminationWorker {
    pub(super) fn reserve_start(&self) -> Result<TerminationWorkerStart<'_>, ServerCloseError> {
        let mut state = self.state.lock();
        if state.status != TerminationWorkerStatus::Idle {
            return Err(ServerCloseError::Reentrant);
        }
        state.status = TerminationWorkerStatus::Starting;
        Ok(TerminationWorkerStart {
            worker: self,
            committed: false,
        })
    }

    pub(super) fn join(&self) -> Result<(), ServerCloseError> {
        let current = std::thread::current().id();
        loop {
            let handle = {
                let mut state = self.state.lock();
                if state.thread_id == Some(current)
                    && matches!(
                        state.status,
                        TerminationWorkerStatus::Running | TerminationWorkerStatus::Joining
                    )
                {
                    return Err(ServerCloseError::Reentrant);
                }

                match state.status {
                    TerminationWorkerStatus::Idle | TerminationWorkerStatus::Joined => {
                        return Ok(());
                    }
                    TerminationWorkerStatus::Starting => {
                        self.changed.wait(&mut state);
                        continue;
                    }
                    TerminationWorkerStatus::Running => {
                        state.status = TerminationWorkerStatus::Joining;
                        state
                            .handle
                            .take()
                            .expect("running RTD termination worker owns a join handle")
                    }
                    TerminationWorkerStatus::Joining => {
                        self.changed.wait(&mut state);
                        continue;
                    }
                }
            };

            let outcome = handle.join();
            let mut state = self.state.lock();
            state.status = TerminationWorkerStatus::Joined;
            state.thread_id = None;
            self.changed.notify_all();
            return outcome.map_err(|_| ServerCloseError::WorkerPanicked);
        }
    }

    pub(super) fn is_idle_or_joined(&self) -> bool {
        matches!(
            self.state.lock().status,
            TerminationWorkerStatus::Idle | TerminationWorkerStatus::Joined
        )
    }
}

impl TerminationWorkerStart<'_> {
    pub(super) fn commit(mut self, handle: std::thread::JoinHandle<()>) {
        let mut state = self.worker.state.lock();
        debug_assert_eq!(state.status, TerminationWorkerStatus::Starting);
        state.thread_id = Some(handle.thread().id());
        state.handle = Some(handle);
        state.status = TerminationWorkerStatus::Running;
        self.committed = true;
        self.worker.changed.notify_all();
    }
}

impl Drop for TerminationWorkerStart<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self.worker.state.lock();
        debug_assert_eq!(state.status, TerminationWorkerStatus::Starting);
        state.status = TerminationWorkerStatus::Idle;
        self.worker.changed.notify_all();
    }
}
