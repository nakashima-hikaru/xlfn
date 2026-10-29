use super::event::{ManualResetEvent, Win32EventError};
use crate::sync::{Condvar, Mutex, MutexGuard};
use rustc_hash::FxHashMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
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
struct ServerOperationState {
    phase: ServerPhase,
    termination_coordinator: Option<ThreadId>,
    deferred_attempt: u64,
    in_flight: usize,
    notifications_in_flight: usize,
    in_flight_by_thread: FxHashMap<ThreadId, usize>,
    notifications_in_flight_by_thread: FxHashMap<ThreadId, usize>,
    resource_teardown_owner: Option<ThreadId>,
}

pub(super) struct ServerOperationBarrier {
    state: Mutex<ServerOperationState>,
    quiescent: ManualResetEvent,
    termination_finished: ManualResetEvent,
    resources_finished: ManualResetEvent,
}

pub(super) struct ServerOperation<'a> {
    barrier: &'a ServerOperationBarrier,
    thread_id: ThreadId,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

/// Thread-affine observation admission anchored by an owned COM reference.
/// Unlike transferred disconnect work, this participates in reentrancy checks.
pub(super) struct OwnedServerObservationCall {
    barrier: NonNull<ServerOperationBarrier>,
    thread_id: ThreadId,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

/// An admitted COM operation transferred to the generation cleanup worker.
/// Its owner must retain a server COM reference until this guard is dropped.
pub(super) struct OwnedServerCall {
    barrier: NonNull<ServerOperationBarrier>,
}

// SAFETY: the retained server reference keeps the synchronized barrier live;
// this guard counts unbound work and has no apartment-affine COM interface.
unsafe impl Send for OwnedServerCall {}

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

/// Closed admission and drained operations/notifications for this exact barrier.
/// The optional termination owner publishes completion when the proof is dropped.
pub(super) struct DrainedServerOperations<'a> {
    barrier: &'a ServerOperationBarrier,
    _termination: Option<ServerTermination<'a>>,
}

impl DrainedServerOperations<'_> {
    pub(super) fn belongs_to(&self, barrier: &ServerOperationBarrier) -> bool {
        std::ptr::eq(self.barrier, barrier)
    }

    pub(super) fn teardown_resources(
        &self,
    ) -> Result<ServerResourceTeardown<'_>, ServerCloseError> {
        self.barrier.acquire_resource_teardown()
    }
}

impl<'a> ServerTermination<'a> {
    pub(super) fn into_drained(self) -> DrainedServerOperations<'a> {
        DrainedServerOperations {
            barrier: self.barrier,
            _termination: Some(self),
        }
    }
}

/// Exclusive backend/callback disposal, including retries after the terminal
/// phase was published by a deferred worker. The operation phase alone cannot
/// serialize those retries because close_and_wait then returns no owner.
pub(super) struct ServerResourceTeardown<'a> {
    barrier: &'a ServerOperationBarrier,
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
    owner: ThreadId,
    attempt: u64,
    state: MutexGuard<'a, ServerOperationState>,
    committed: bool,
}

/// Holds the reservation lock until the worker handle has been installed.
pub(super) struct PreparedDeferredTermination<'a> {
    reservation: DeferredTerminationReservation<'a>,
    committed: triomphe::Arc<AtomicBool>,
}

/// One affine right to finish one deferred termination on its issuing barrier.
/// The owned barrier reference covers the wait and the complete callback.
pub(super) struct DeferredTerminationTicket {
    barrier: triomphe::Arc<ServerOperationBarrier>,
    owner: ThreadId,
    attempt: u64,
    committed: triomphe::Arc<AtomicBool>,
}

impl DeferredTerminationTicket {
    pub(super) fn with_completion<R>(
        self,
        complete: impl FnOnce(ServerTermination<'_>) -> R,
    ) -> Result<R, ServerCloseError> {
        // Only another thread can wait while the reserving thread still owns
        // the startup mutex. Reject self-wait before touching that mutex.
        if self.owner == std::thread::current().id() && !self.committed.load(Ordering::Acquire) {
            return Err(ServerCloseError::Reentrant);
        }
        let termination = self
            .barrier
            .wait_for_deferred_termination(self.owner, self.attempt)?;
        Ok(complete(termination))
    }
}

impl PreparedDeferredTermination<'_> {
    pub(super) fn commit(self) {
        self.reservation.commit();
        // The mutex guard is gone before same-thread completion is enabled.
        self.committed.store(true, Ordering::Release);
    }
}

impl ServerOperationBarrier {
    pub(super) fn phase(&self) -> ServerPhase {
        self.state.lock().phase
    }

    #[cfg(test)]
    pub(super) fn try_phase(&self) -> Option<ServerPhase> {
        self.state.try_lock().map(|state| state.phase)
    }

    #[cfg(test)]
    pub(super) fn in_flight(&self) -> usize {
        self.state.lock().in_flight
    }

    pub(super) fn new() -> Result<Self, Win32EventError> {
        Ok(Self {
            state: Mutex::new(ServerOperationState::default()),
            quiescent: ManualResetEvent::new(false)?,
            termination_finished: ManualResetEvent::new(false)?,
            resources_finished: ManualResetEvent::new(true)?,
        })
    }

    fn acquire_resource_teardown(&self) -> Result<ServerResourceTeardown<'_>, ServerCloseError> {
        let current = std::thread::current().id();
        loop {
            {
                let mut state = self.state.lock();
                debug_assert_ne!(state.phase, ServerPhase::Open);
                match state.resource_teardown_owner {
                    Some(owner) if owner == current => return Err(ServerCloseError::Reentrant),
                    Some(_) => {}
                    None => {
                        self.resources_finished
                            .reset()
                            .map_err(ServerCloseError::Event)?;
                        state.resource_teardown_owner = Some(current);
                        return Ok(ServerResourceTeardown {
                            barrier: self,
                            _not_send_or_sync: PhantomData,
                        });
                    }
                }
            }
            self.resources_finished
                .wait_with_com_pumping()
                .map_err(ServerCloseError::WaitFailed)?;
        }
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

    /// # Safety
    /// The caller must retain the server owning this barrier until the
    /// returned thread-affine admission has been dropped.
    pub(super) unsafe fn enter_observation(&self) -> Option<OwnedServerObservationCall> {
        let thread_id = std::thread::current().id();
        let mut state = self.state.lock();
        if state.phase != ServerPhase::Open {
            return None;
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .expect("RTD observation operation count cannot overflow");
        let per_thread = state.in_flight_by_thread.entry(thread_id).or_default();
        *per_thread = per_thread
            .checked_add(1)
            .expect("per-thread RTD observation operation count cannot overflow");
        Some(OwnedServerObservationCall {
            barrier: NonNull::from(self),
            thread_id,
            _not_send_or_sync: PhantomData,
        })
    }

    /// # Safety
    /// The caller must keep this barrier allocated through the returned
    /// guard's destruction, including when it completes on another thread.
    pub(super) unsafe fn enter_owned(&self) -> Option<OwnedServerCall> {
        let mut state = self.state.lock();
        if state.phase != ServerPhase::Open {
            return None;
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .expect("RTD COM operation count cannot overflow");
        Some(OwnedServerCall {
            barrier: NonNull::from(self),
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
        if deferred {
            state.deferred_attempt = state
                .deferred_attempt
                .checked_add(1)
                .ok_or(ServerCloseError::Reentrant)?;
        }
        let attempt = state.deferred_attempt;
        state.termination_coordinator = None;
        state.phase = ServerPhase::Terminating { owner, deferred };

        if deferred {
            Ok(ServerTerminationRequest::Deferred(
                DeferredTerminationReservation {
                    barrier: self,
                    owner,
                    attempt,
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

    fn wait_for_deferred_termination(
        &self,
        owner: ThreadId,
        attempt: u64,
    ) -> Result<ServerTermination<'_>, ServerCloseError> {
        loop {
            {
                let mut state = self.state.lock();
                match state.phase {
                    ServerPhase::Terminating {
                        owner: active_owner,
                        deferred: true,
                    } if active_owner == owner && state.deferred_attempt == attempt => {
                        if state.termination_coordinator.is_some() {
                            return Err(ServerCloseError::Reentrant);
                        }
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

    pub(super) fn drain(&self) -> Result<DrainedServerOperations<'_>, ServerCloseError> {
        let termination = self.close_and_wait()?;
        Ok(DrainedServerOperations {
            barrier: self,
            _termination: termination,
        })
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

impl<'a> DeferredTerminationReservation<'a> {
    pub(super) fn prepare(
        self,
        barrier: triomphe::Arc<ServerOperationBarrier>,
    ) -> Result<(PreparedDeferredTermination<'a>, DeferredTerminationTicket), ServerCloseError>
    {
        if !std::ptr::eq(self.barrier, barrier.as_ref()) {
            return Err(ServerCloseError::Reentrant);
        }
        let committed = triomphe::Arc::new(AtomicBool::new(false));
        let ticket = DeferredTerminationTicket {
            barrier,
            owner: self.owner,
            attempt: self.attempt,
            committed: triomphe::Arc::clone(&committed),
        };
        Ok((
            PreparedDeferredTermination {
                reservation: self,
                committed,
            },
            ticket,
        ))
    }

    fn commit(mut self) {
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

impl Drop for OwnedServerObservationCall {
    fn drop(&mut self) {
        // SAFETY: the owning observation retains its server reference until
        // this admission has been released. Reuse the thread-affine exit path.
        let barrier = unsafe { self.barrier.as_ref() };
        drop(ServerOperation {
            barrier,
            thread_id: self.thread_id,
            _not_send_or_sync: PhantomData,
        });
    }
}

impl Drop for OwnedServerCall {
    fn drop(&mut self) {
        // SAFETY: the owner retains its server reference through this Drop.
        let barrier = unsafe { self.barrier.as_ref() };
        let mut state = barrier.state.lock();
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("RTD transferred COM operation count remains balanced");
        if state.in_flight == 0
            && state.notifications_in_flight == 0
            && matches!(state.phase, ServerPhase::Terminating { .. })
        {
            barrier
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

impl Drop for ServerResourceTeardown<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.state.lock();
        debug_assert_eq!(
            state.resource_teardown_owner,
            Some(std::thread::current().id())
        );
        state.resource_teardown_owner = None;
        self.barrier
            .resources_finished
            .set()
            .unwrap_or_else(|_| std::process::abort());
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminationWorkerStatus {
    Idle,
    Starting,
    Running,
    Joining,
    Joined,
}

#[derive(Default)]
enum TerminationWorkerPhase {
    #[default]
    Idle,
    Starting {
        owner: ThreadId,
    },
    Running(std::thread::JoinHandle<()>),
    // The joiner owns the handle outside the lock. Keep its worker identity
    // here so that worker cannot wait for the joiner that is waiting for it.
    Joining {
        worker_thread: ThreadId,
    },
    Joined,
}

#[derive(Default)]
struct TerminationWorkerState {
    phase: TerminationWorkerPhase,
    // Closed before joining, including when no worker has ever started.
    sealed: bool,
}

#[derive(Default)]
pub(super) struct TerminationWorker {
    state: Mutex<TerminationWorkerState>,
    changed: Condvar,
}

pub(super) struct TerminationWorkerStart<'a> {
    worker: &'a TerminationWorker,
    committed: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl TerminationWorker {
    pub(super) fn reserve_start(&self) -> Result<TerminationWorkerStart<'_>, ServerCloseError> {
        let mut state = self.state.lock();
        if state.sealed || !matches!(state.phase, TerminationWorkerPhase::Idle) {
            return Err(ServerCloseError::Reentrant);
        }
        state.phase = TerminationWorkerPhase::Starting {
            owner: std::thread::current().id(),
        };
        Ok(TerminationWorkerStart {
            worker: self,
            committed: false,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Prevent any later reservation before checking or joining the worker.
    /// A pre-existing Starting reservation must still commit or roll back; join
    /// waits for it, and rollback cannot reopen this sealed worker.
    pub(super) fn seal_and_join(&self) -> Result<(), ServerCloseError> {
        self.state.lock().sealed = true;
        self.join()
    }

    pub(super) fn join(&self) -> Result<(), ServerCloseError> {
        let current = std::thread::current().id();
        loop {
            let handle = {
                let mut state = self.state.lock();
                let worker_thread = match &state.phase {
                    // The thread-affine reservation must commit or roll back
                    // before its owner can wait for Starting to finish.
                    TerminationWorkerPhase::Starting { owner } => Some(*owner),
                    TerminationWorkerPhase::Running(handle) => Some(handle.thread().id()),
                    TerminationWorkerPhase::Joining { worker_thread } => Some(*worker_thread),
                    _ => None,
                };
                if worker_thread == Some(current) {
                    return Err(ServerCloseError::Reentrant);
                }

                match &state.phase {
                    TerminationWorkerPhase::Idle => {
                        if state.sealed {
                            state.phase = TerminationWorkerPhase::Joined;
                        }
                        return Ok(());
                    }
                    TerminationWorkerPhase::Joined => return Ok(()),
                    TerminationWorkerPhase::Starting { .. } => {
                        self.changed.wait(&mut state);
                        continue;
                    }
                    TerminationWorkerPhase::Running(handle) => {
                        let joining = TerminationWorkerPhase::Joining {
                            worker_thread: handle.thread().id(),
                        };
                        let TerminationWorkerPhase::Running(handle) =
                            std::mem::replace(&mut state.phase, joining)
                        else {
                            xlfn_kernel::invariant::fail_stop();
                        };
                        handle
                    }
                    TerminationWorkerPhase::Joining { .. } => {
                        self.changed.wait(&mut state);
                        continue;
                    }
                }
            };

            let outcome = crate::panic_boundary::contain_panic(handle.join());
            let mut state = self.state.lock();
            state.phase = TerminationWorkerPhase::Joined;
            self.changed.notify_all();
            return outcome.map_err(|_| ServerCloseError::WorkerPanicked);
        }
    }

    pub(super) fn is_idle_or_joined(&self) -> bool {
        matches!(
            self.state.lock().phase,
            TerminationWorkerPhase::Idle | TerminationWorkerPhase::Joined
        )
    }

    #[cfg(test)]
    pub(super) fn is_sealed(&self) -> bool {
        self.state.lock().sealed
    }

    #[cfg(test)]
    pub(super) fn phase(&self) -> TerminationWorkerStatus {
        match &self.state.lock().phase {
            TerminationWorkerPhase::Idle => TerminationWorkerStatus::Idle,
            TerminationWorkerPhase::Starting { .. } => TerminationWorkerStatus::Starting,
            TerminationWorkerPhase::Running(_) => TerminationWorkerStatus::Running,
            TerminationWorkerPhase::Joining { .. } => TerminationWorkerStatus::Joining,
            TerminationWorkerPhase::Joined => TerminationWorkerStatus::Joined,
        }
    }
}

impl TerminationWorkerStart<'_> {
    pub(super) fn commit(mut self, handle: std::thread::JoinHandle<()>) {
        let mut state = self.worker.state.lock();
        debug_assert!(matches!(
            state.phase,
            TerminationWorkerPhase::Starting { .. }
        ));
        state.phase = TerminationWorkerPhase::Running(handle);
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
        debug_assert!(matches!(
            state.phase,
            TerminationWorkerPhase::Starting { .. }
        ));
        state.phase = TerminationWorkerPhase::Idle;
        self.worker.changed.notify_all();
    }
}

#[cfg(test)]
mod deferred_ticket_tests {
    use super::*;
    use triomphe::Arc;

    #[test]
    fn deferred_ticket_issues_one_completion_owner() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(reservation) =
            barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        let (prepared, ticket) = reservation.prepare(Arc::clone(&barrier)).unwrap();
        let owner = std::thread::current().id();
        let attempt = ticket.attempt;
        prepared.commit();
        drop(operation);
        ticket
            .with_completion(|termination| {
                // Even an internal second acquisition cannot overwrite the live
                // coordinator or mint another owner while the first is retained.
                assert!(matches!(
                    barrier.wait_for_deferred_termination(owner, attempt),
                    Err(ServerCloseError::Reentrant)
                ));
                assert_eq!(
                    barrier.phase(),
                    ServerPhase::Terminating {
                        owner,
                        deferred: true
                    }
                );
                drop(termination);
            })
            .unwrap();
        assert_eq!(barrier.phase(), ServerPhase::Terminated);
        assert!(barrier.drain().is_ok());
        assert!(barrier.drain().is_ok());
    }

    #[test]
    fn deferred_ticket_rejects_reserving_thread_wait_before_commit() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(reservation) =
            barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        let (prepared, ticket) = reservation.prepare(Arc::clone(&barrier)).unwrap();
        assert!(matches!(
            ticket.with_completion(|_| panic!("startup lock is still owned")),
            Err(ServerCloseError::Reentrant)
        ));
        drop(prepared);
        drop(operation);
        assert_eq!(barrier.phase(), ServerPhase::Open);
    }

    #[test]
    fn deferred_ticket_worker_can_wait_while_spawn_commit_is_pending() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(reservation) =
            barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        let (prepared, ticket) = reservation.prepare(Arc::clone(&barrier)).unwrap();
        let (started, waiting) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            ticket.with_completion(|owner| drop(owner))
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(barrier.try_phase().is_none());
        prepared.commit();
        drop(operation);
        worker.join().unwrap().unwrap();
        assert_eq!(barrier.phase(), ServerPhase::Terminated);
    }

    #[test]
    fn deferred_ticket_preparation_rejects_a_substituted_barrier() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let other = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(reservation) =
            barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        assert!(reservation.prepare(Arc::clone(&other)).is_err());
        assert_eq!(barrier.phase(), ServerPhase::Open);
        assert_eq!(other.phase(), ServerPhase::Open);
        drop(operation);
    }

    #[test]
    fn rolled_back_ticket_cannot_claim_a_later_attempt_on_the_same_thread() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(first) = barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        let (abandoned, stale) = first.prepare(Arc::clone(&barrier)).unwrap();
        drop(abandoned);
        let ServerTerminationRequest::Deferred(second) = barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination again");
        };
        let (prepared, current) = second.prepare(Arc::clone(&barrier)).unwrap();
        assert_ne!(stale.attempt, current.attempt);
        prepared.commit();
        drop(operation);
        // The old ticket runs on a worker thread, so the generation check
        // (rather than the reserving-thread precommit check) rejects it.
        let rejected = std::thread::spawn(move || {
            stale.with_completion(|_| panic!("stale ticket stole the later attempt"))
        });
        assert!(matches!(
            rejected.join().unwrap(),
            Err(ServerCloseError::Reentrant)
        ));
        current.with_completion(|owner| drop(owner)).unwrap();
        assert_eq!(barrier.phase(), ServerPhase::Terminated);
    }

    #[test]
    fn uncommitted_prepared_spawn_rolls_back_and_invalidates_ticket() {
        let barrier = Arc::new(ServerOperationBarrier::default());
        let operation = barrier.enter().unwrap();
        let ServerTerminationRequest::Deferred(reservation) =
            barrier.request_termination().unwrap()
        else {
            panic!("active operation must defer termination");
        };
        let (prepared, ticket) = reservation.prepare(Arc::clone(&barrier)).unwrap();
        drop(prepared);
        assert_eq!(barrier.phase(), ServerPhase::Open);
        drop(operation);
        assert!(matches!(
            ticket.with_completion(|_| panic!("rolled back ticket must not complete")),
            Err(ServerCloseError::Reentrant)
        ));
        assert!(barrier.enter().is_some());
    }
}
