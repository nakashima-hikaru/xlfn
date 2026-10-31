use super::calculation::{
    CalculationAdmission, CalculationEpoch, CalculationPin, CalculationState,
};
use super::control::{CloseAttempt, ControlPhase, RegistryControl};
use super::task::{ActiveReservation, AsyncTask, TaskControl, TaskControlBatch, TrackedFuture};
use crate::cancellation::CancellationSource;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::future::Future;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use xlfn_kernel::drain_gate::DrainGate;
use xlfn_kernel::published_owner::PublishedOwner;

/// Framework task ownership, independent of any executor implementation.
///
/// Calculation pins retain their allocation, while active task accounting
/// retains this registry. `activity` additionally drains the final release
/// tail before its unique owner can reclaim the registry.
pub(crate) struct TaskRegistry {
    pub(super) active_tasks: AtomicUsize,
    pub(crate) next_id: AtomicU64,
    limit: AtomicUsize,
    closing: AtomicBool,
    current: AtomicPtr<CalculationState>,
    // Scalar observation never dereferences a reclaimable calculation. Writers
    // publish it with current under publication; snapshots still pin under that
    // mutex and cannot observe the intermediate pointer/epoch update.
    published_epoch: AtomicU64,
    publication: Mutex<()>,
    transition: Mutex<()>,
    pub(super) control: Mutex<RegistryControl>,
    pub(super) activity: DrainGate,
    pub(super) idle_lock: Mutex<()>,
    pub(super) idle: Condvar,
    pub(crate) observer: crate::shutdown_trace::ObservationSink,
    #[cfg(test)]
    pub(crate) after_publish_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) before_transition_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_snapshot_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_admission_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone, Copy)]
pub(crate) struct RegistryPtr(NonNull<TaskRegistry>);

impl RegistryPtr {
    pub(crate) fn from_ref(registry: &TaskRegistry) -> Self {
        Self(NonNull::from(registry))
    }

    /// # Safety
    /// The registry's unique owner must remain alive through this borrow.
    /// Task/reservation activity permits and runtime publication readers
    /// establish that lifetime; shutdown drains them before reclamation.
    pub(crate) unsafe fn get(self) -> &'static TaskRegistry {
        // SAFETY: delegated to the caller's ownership witness.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: TaskRegistry is Sync and the transferring task retains its activity permit.
unsafe impl Send for RegistryPtr {}
// SAFETY: shared access is synchronized; capabilities do not own/reclaim the registry.
unsafe impl Sync for RegistryPtr {}

pub(crate) struct TaskReservation<'a> {
    registry: RegistryPtr,
    id: u64,
    // Destruction releases admission before the allocation pin and activity.
    admission: CalculationAdmission<'a>,
    calculation: CalculationPin,
    active: ActiveReservation<'a>,
}

/// Captures the calculation identity and its allocation in one publication read.
///
/// Preparation may reenter calculation transitions: this pin retains the
/// allocation without entering calculation admission or registering a task.
/// The registry borrow also prevents its unique owner from being reclaimed.
pub(crate) struct CalculationSnapshot<'a> {
    registry: &'a TaskRegistry,
    calculation: CalculationPin,
}

impl CalculationSnapshot<'_> {
    pub(crate) fn epoch(&self) -> CalculationEpoch {
        self.calculation.get().epoch
    }

    pub(crate) fn belongs_to(&self, registry: &TaskRegistry) -> bool {
        std::ptr::eq(registry, self.registry)
    }
}

impl TaskReservation<'_> {
    pub(crate) fn commit<F>(
        self,
        future: F,
        cancellation: CancellationSource,
    ) -> AsyncTask<TrackedFuture<F>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let registration = cancellation.task_registration();
        self.admission
            .insert_task(self.id, TaskControl { cancellation });
        let completion = self.active.commit(self.calculation, self.id);
        // SAFETY: the completion guard now owns registry activity, and the
        // reservation still owns calculation admission during this access.
        let registry = unsafe { self.registry.get() };
        registry
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::StartAsyncTask);
        drop(self.admission);
        AsyncTask::new(TrackedFuture::new(future, registration, completion))
    }
}

/// Owns one close attempt without holding a mutex across callbacks or waits.
struct CloseClaim<'a> {
    registry: &'a TaskRegistry,
    attempt: Option<CloseAttempt>,
}

impl CloseClaim<'_> {
    fn finish(mut self) {
        let attempt = self.attempt.take().expect("live close claim");
        self.registry.control.lock().finish_close(attempt);
    }
}

impl Drop for CloseClaim<'_> {
    fn drop(&mut self) {
        if let Some(attempt) = self.attempt.take() {
            self.registry.control.lock().abandon_close(attempt);
        }
    }
}

pub(crate) struct CancellationPending {
    tasks: TaskControlBatch,
}

impl Drop for CancellationPending {
    fn drop(&mut self) {
        // Abandonment also cancels controls outside every registry lock.
        cancel_tasks(std::mem::take(&mut self.tasks));
    }
}

pub(crate) struct TasksCancelled;
pub(crate) struct TasksDrained;

/// Only cancellation followed by task/release-tail drain grants finish authority.
#[must_use = "complete the close or drop it to leave Closing available for retry"]
pub(crate) struct RegistryClose<'a, S> {
    // Cancellation runs before relinquishing the unique attempt on Drop.
    state: S,
    claim: CloseClaim<'a>,
}

impl<'a> RegistryClose<'a, CancellationPending> {
    pub(crate) fn cancel(self) -> RegistryClose<'a, TasksCancelled> {
        let Self { state, claim } = self;
        drop(state);
        RegistryClose {
            state: TasksCancelled,
            claim,
        }
    }
}

impl<'a> RegistryClose<'a, TasksCancelled> {
    pub(crate) fn wait(self) -> RegistryClose<'a, TasksDrained> {
        self.claim.registry.wait_idle();
        RegistryClose {
            state: TasksDrained,
            claim: self.claim,
        }
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn wait_timeout(
        self,
        timeout: std::time::Duration,
    ) -> XllResult<RegistryClose<'a, TasksDrained>> {
        if !self.claim.registry.wait_idle_timeout(timeout) {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_TIME,
            });
        }
        Ok(RegistryClose {
            state: TasksDrained,
            claim: self.claim,
        })
    }
}

impl RegistryClose<'_, TasksDrained> {
    pub(crate) fn finish(self) {
        self.claim.finish();
    }
}

impl TaskRegistry {
    pub(crate) fn new() -> Self {
        let initial = PublishedOwner::new(CalculationState::new(CalculationEpoch::INITIAL));
        initial.seal_admission();
        let current = NonNull::from(initial.as_ref());
        Self {
            active_tasks: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
            limit: AtomicUsize::new(0),
            closing: AtomicBool::new(true),
            current: AtomicPtr::new(current.as_ptr()),
            published_epoch: AtomicU64::new(CalculationEpoch::INITIAL.get()),
            publication: Mutex::new(()),
            transition: Mutex::new(()),
            control: Mutex::new(RegistryControl::new(initial)),
            activity: DrainGate::new_sealed(),
            idle_lock: Mutex::new(()),
            idle: Condvar::new(),
            observer: crate::shutdown_trace::ObservationSink::new(),
            #[cfg(test)]
            after_publish_hook: Mutex::new(None),
            #[cfg(test)]
            before_transition_hook: Mutex::new(None),
            #[cfg(test)]
            after_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            after_admission_hook: Mutex::new(None),
        }
    }

    pub(crate) fn current_epoch(&self) -> CalculationEpoch {
        CalculationEpoch::new(self.published_epoch.load(Ordering::Acquire))
            .expect("published calculation epochs are non-zero")
    }

    // The caller holds publication and has installed this state's owner in
    // control. The scalar store is the epoch observation's linearization point;
    // it grants no calculation admission or allocation lifetime authority.
    fn publish_current(&self, pointer: NonNull<CalculationState>, epoch: CalculationEpoch) {
        self.current.store(pointer.as_ptr(), Ordering::Release);
        self.published_epoch.store(epoch.get(), Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> CalculationSnapshot<'_> {
        let calculation = {
            let _publication = self.publication.lock();
            // SAFETY: publication excludes reclamation of the current owner.
            let current = unsafe { &*self.current.load(Ordering::Acquire) };
            // SAFETY: publication retains the owner while pinning, and the
            // returned snapshot borrows the registry until the pin drops.
            unsafe { CalculationPin::acquire(current) }
        };
        #[cfg(test)]
        {
            let hook = self.after_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        CalculationSnapshot {
            registry: self,
            calculation,
        }
    }

    pub(crate) fn start(&self, limit: crate::addin::AsyncTaskLimit) -> XllResult<()> {
        let _transition = self.transition.lock();
        let mut control = self.control.lock();
        if control.phase() != ControlPhase::Stopped {
            return Err(XllError::Closing);
        }
        let _publication = self.publication.lock();
        // SAFETY: the publication lock protects the current allocation.
        let current = unsafe { &*self.current.load(Ordering::Acquire) };
        // An uncommitted preparation snapshot is not an active task. Close
        // may finish while it retains the closed calculation, but reopening
        // must not replace that same-epoch owner until its final pin drops.
        if current.pin_count() != 0 {
            return Err(XllError::Closing);
        }
        let epoch = current.epoch;
        let state = PublishedOwner::new(CalculationState::new(epoch));
        let pointer = NonNull::from(state.as_ref());
        control.calculations.insert(epoch, state);
        control
            .calculations
            .retain(|id, state| *id == epoch || state.pin_count() != 0);
        self.publish_current(pointer, epoch);
        self.limit.store(limit.get(), Ordering::Relaxed);
        self.closing.store(false, Ordering::Release);
        drop(
            self.activity
                .control()
                .wait_if_sealed()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
                .reopen(),
        );
        control.start();
        Ok(())
    }

    pub(crate) fn preflight_available(&self) -> XllResult<bool> {
        if self.closing.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        Ok(self.active_tasks.load(Ordering::Relaxed) < self.limit.load(Ordering::Relaxed))
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn preflight(&self, epoch: CalculationEpoch) -> XllResult<()> {
        if self.preflight_available()? {
            return Ok(());
        }
        // Only saturation takes authoritative calculation admission and
        // capacity reservation before potentially expensive Excel conversion.
        drop(self.reserve(epoch)?);
        Ok(())
    }

    pub(crate) fn preflight_snapshot(&self, snapshot: &CalculationSnapshot<'_>) -> XllResult<()> {
        if !std::ptr::eq(self, snapshot.registry) {
            return Err(XllError::Closing);
        }
        if self.preflight_available()? {
            return Ok(());
        }
        // A saturated call still validates calculation admission before
        // reporting capacity exhaustion. Borrow the existing pin so neither
        // a second publication lock nor an additional pin is needed.
        let (admission, active, _) = self.admit(snapshot.calculation.get())?;
        drop(active);
        drop(admission);
        Ok(())
    }

    #[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
    pub(crate) fn reserve(&self, epoch: CalculationEpoch) -> XllResult<TaskReservation<'_>> {
        if self.closing.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        let snapshot = self.snapshot();
        if snapshot.epoch() != epoch {
            return Err(cancelled_calculation_error());
        }
        self.reserve_snapshot(snapshot)
    }

    pub(crate) fn reserve_snapshot<'a>(
        &'a self,
        snapshot: CalculationSnapshot<'a>,
    ) -> XllResult<TaskReservation<'a>> {
        if !std::ptr::eq(self, snapshot.registry) {
            return Err(XllError::Closing);
        }
        let calculation = snapshot.calculation;
        // SAFETY: this pin moves into the returned reservation and retains
        // the allocation until after its admission guard is destroyed.
        let current = unsafe { calculation.pointer().as_ref() };
        let (admission, active, id) = self.admit(current)?;
        Ok(TaskReservation {
            registry: RegistryPtr::from_ref(self),
            id,
            admission,
            calculation,
            active,
        })
    }

    // Saturated preflight needs only the error tag. Keep the admission tuple
    // local to its caller instead of materializing a large Result on the stack.
    #[inline(always)]
    fn admit<'a>(
        &'a self,
        current: &'a CalculationState,
    ) -> XllResult<(CalculationAdmission<'a>, ActiveReservation<'a>, u64)> {
        if self.closing.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        // Advance/cancel seals the old state before publishing its successor.
        // Retaining a snapshot keeps the allocation alive, but never grants
        // admission to a retired calculation.
        let admission = current.enter_admission().map_err(|_| {
            if self.closing.load(Ordering::Acquire) {
                XllError::Closing
            } else {
                cancelled_calculation_error()
            }
        })?;
        #[cfg(test)]
        {
            let hook = self.after_admission_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let active = ActiveReservation::try_acquire(self).ok_or(XllError::Overloaded)?;
        let id = self
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| XllError::Overloaded)?;
        Ok((admission, active, id))
    }

    pub(crate) fn cancel_calculation(&self, epoch: CalculationEpoch) -> TaskControlBatch {
        let control = self.control.lock();
        let Some(state) = control.calculations.get(&epoch) else {
            return TaskControlBatch::new();
        };
        state.close_admission().wait().drain_tasks()
    }

    pub(crate) fn advance(&self) -> bool {
        #[cfg(test)]
        {
            let hook = self.before_transition_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let _transition = self.transition.lock();
        let old = {
            let mut control = self.control.lock();
            if matches!(
                control.phase(),
                ControlPhase::Closing | ControlPhase::Advancing { .. }
            ) || (control.phase() == ControlPhase::Running
                && self.closing.load(Ordering::Acquire))
            {
                return false;
            }
            let _publication = self.publication.lock();
            // SAFETY: publication excludes state reclamation while pinning.
            let current = unsafe { &*self.current.load(Ordering::Acquire) };
            let Some(next) = current.epoch.checked_next() else {
                return false;
            };
            if control.phase() == ControlPhase::Stopped {
                let state = PublishedOwner::new(CalculationState::new(next));
                state.seal_admission();
                let pointer = NonNull::from(state.as_ref());
                control.calculations.insert(next, state);
                self.publish_current(pointer, next);
                control
                    .calculations
                    .retain(|id, state| *id == next || state.pin_count() != 0);
                return true;
            }
            current.seal_admission();
            control.begin_advance(current.epoch, next);
            // SAFETY: the publication lock retains the owner while pinning.
            unsafe { CalculationPin::acquire(current) }
        };
        let _drained_admission = old.get().close_admission().wait();
        let mut control = self.control.lock();
        let ControlPhase::Advancing { from, to } = control.phase() else {
            return false;
        };
        if self.closing.load(Ordering::Acquire) {
            // Admission was sealed while the old calculation drained. No
            // replacement was published; leave a stable control state so the
            // subsequent close can drain the existing calculations.
            control.finish_advance();
            return false;
        }
        if from != old.get().epoch {
            xlfn_kernel::invariant::fail_stop();
        }
        let _publication = self.publication.lock();
        let state = PublishedOwner::new(CalculationState::new(to));
        let pointer = NonNull::from(state.as_ref());
        control.calculations.insert(to, state);
        self.publish_current(pointer, to);
        drop(old);
        control
            .calculations
            .retain(|id, state| *id == to || state.pin_count() != 0);
        control.finish_advance();
        drop(_publication);
        drop(control);
        #[cfg(test)]
        {
            let hook = self.after_publish_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        true
    }

    pub(crate) fn close_admission(&self) {
        self.closing.store(true, Ordering::Release);
    }

    pub(crate) fn request_close(&self) -> XllResult<RegistryClose<'_, CancellationPending>> {
        // Declare the claim first so an unwind releases the control lock before
        // abandoning the attempt. No lock is retained across cancellation callbacks.
        let mut claim = CloseClaim {
            registry: self,
            attempt: None,
        };
        let mut control = self.control.lock();
        claim.attempt = Some(control.begin_close()?);
        self.closing.store(true, Ordering::Release);
        drop(self.activity.control().seal());
        for state in control.calculations.values() {
            state.seal_admission();
        }
        let mut tasks = TaskControlBatch::new();
        for state in control.calculations.values() {
            tasks.extend(state.close_admission().wait().drain_tasks());
        }
        drop(control);
        Ok(RegistryClose {
            state: CancellationPending { tasks },
            claim,
        })
    }

    pub(crate) fn wait_idle(&self) {
        let mut lock = self.idle_lock.lock();
        while self.active_tasks.load(Ordering::Acquire) != 0 {
            self.idle.wait(&mut lock);
        }
        drop(lock);
        drop(self.activity.control().wait_idle());
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn wait_idle_timeout(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut lock = self.idle_lock.lock();
        while self.active_tasks.load(Ordering::Acquire) != 0 {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            self.idle.wait_for(&mut lock, deadline - now);
        }
        drop(lock);
        drop(self.activity.control().wait_idle());
        true
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_stopped_epoch(&self, epoch: CalculationEpoch) {
        let mut control = self.control.lock();
        assert_eq!(control.phase(), ControlPhase::Stopped);
        let _publication = self.publication.lock();
        assert!(
            control
                .calculations
                .values()
                .all(|state| state.pin_count() == 0)
        );
        let state = PublishedOwner::new(CalculationState::new(epoch));
        state.seal_admission();
        let pointer = NonNull::from(state.as_ref());
        control.calculations.clear();
        control.calculations.insert(epoch, state);
        self.publish_current(pointer, epoch);
    }

    pub(crate) fn capacity(&self) -> usize {
        self.limit.load(Ordering::Relaxed)
    }
}

pub(crate) fn cancelled_calculation_error() -> XllError {
    XllError::ExcelValue(crate::ExcelError::NotAvailable)
}

pub(crate) fn cancel_tasks(tasks: TaskControlBatch) {
    for TaskControl { cancellation } in tasks {
        cancel_source_no_unwind(&cancellation);
        let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            drop(cancellation);
        }));
    }
}

pub(crate) fn cancel_source_no_unwind(source: &CancellationSource) {
    let _ =
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| source.cancel()));
}

#[cfg(test)]
mod epoch_projection_tests {
    use super::*;

    fn assert_epoch(registry: &TaskRegistry, expected: CalculationEpoch) {
        assert_eq!(registry.current_epoch(), expected);
        assert_eq!(registry.snapshot().epoch(), expected);
    }

    #[cfg(not(miri))]
    #[test]
    fn epoch_observation_does_not_wait_for_publication_mutex() {
        let registry = TaskRegistry::new();
        let result = std::thread::scope(|scope| {
            let publication = registry.publication.lock();
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let observed = &registry;
            scope.spawn(move || send.send(observed.current_epoch()).unwrap());
            let result = receive.recv_timeout(std::time::Duration::from_secs(5));
            // Release before scoped joining, including the regression case.
            drop(publication);
            result
        });
        assert_eq!(result.unwrap(), CalculationEpoch::INITIAL);
    }

    #[test]
    fn miri_epoch_projection_follows_stopped_running_and_restart_publications() {
        let registry = TaskRegistry::new();
        let initial = CalculationEpoch::INITIAL;
        assert_epoch(&registry, initial);
        assert!(registry.advance());
        let stopped = initial.checked_next().unwrap();
        assert_epoch(&registry, stopped);
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        assert_epoch(&registry, stopped);

        let old_snapshot = registry.snapshot();
        assert!(registry.advance());
        let running = stopped.checked_next().unwrap();
        assert_epoch(&registry, running);
        assert_eq!(old_snapshot.epoch(), stopped);
        drop(old_snapshot);

        let retained = registry.snapshot();
        registry.request_close().unwrap().cancel().wait().finish();
        assert!(registry.start(crate::AsyncTaskLimit::DEFAULT).is_err());
        assert_epoch(&registry, running);
        drop(retained);
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        assert_epoch(&registry, running);
        registry.request_close().unwrap().cancel().wait().finish();
    }

    #[cfg(feature = "async-builtin")]
    #[test]
    fn miri_epoch_projection_preserves_the_terminal_epoch_on_overflow() {
        let registry = TaskRegistry::new();
        let terminal = CalculationEpoch::new(u64::MAX).unwrap();
        registry.set_stopped_epoch(terminal);
        assert_epoch(&registry, terminal);
        assert!(!registry.advance());
        assert_epoch(&registry, terminal);
        registry.start(crate::AsyncTaskLimit::DEFAULT).unwrap();
        assert!(!registry.advance());
        assert_epoch(&registry, terminal);
        registry.request_close().unwrap().cancel().wait().finish();
    }
}
