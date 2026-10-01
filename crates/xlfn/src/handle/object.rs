//! Runtime-owned arena for formula-handle payloads.
//!
//! The arena is the unique owner of every [`ObjectCell`]. Bindings and pins
//! carry counted, non-owning capabilities. The registry owns the arena and
//! drains every capability before reclaiming it. Hot lookup uses projections.
//! An object is reclaimed after both counts reach
//! zero, and its application destructor always runs outside the arena lock.

use super::token::ObjectId;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;

/// A type-checked, non-owning projection into an [`ObjectCell`].
pub(crate) struct TypedObjectProjection<T: 'static> {
    pointer: NonNull<T>,
}

impl<T: 'static> Copy for TypedObjectProjection<T> {}

impl<T: 'static> Clone for TypedObjectProjection<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> TypedObjectProjection<T> {
    #[cfg(test)]
    pub(crate) fn addr(&self) -> usize {
        self.pointer.addr().get()
    }

    /// Returns a shared reference to the projected object.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the object's backing cell has not been reclaimed
    /// and remains pinned, leased, or protected by an active read capability for the
    /// entire duration of the returned borrow.
    #[inline]
    pub(crate) unsafe fn as_ref_unchecked(&self) -> &T {
        // SAFETY: guaranteed by the caller per contract.
        unsafe { self.pointer.as_ref() }
    }
}

pub(crate) struct HandleCleanupState {
    failure: Mutex<Option<XllError>>,
}

impl HandleCleanupState {
    fn new() -> Self {
        Self {
            failure: Mutex::new(None),
        }
    }

    fn record(&self, error: XllError) {
        let mut failure = self.failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    pub(crate) fn result(&self) -> XllResult<()> {
        self.failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }
}

// Per-object counts have the same portable ceiling on i686 and 64-bit hosts.
// Admission reports Overflow before either count can wrap. Aggregate arena
// counts remain native-sized because they span multiple objects.
type ObjectBindingCount = u32;
type ObjectPinCount = u32;

struct ObjectEntry {
    cell: PublishedOwner<ObjectCell>,
    bindings: ObjectBindingCount,
    pins: ObjectPinCount,
}

struct ObjectArenaState {
    objects: FxHashMap<ObjectId, ObjectEntry>,
    active_pins: usize,
    active_releases: usize,
    sealed: bool,
}

/// Unique owner and reclamation authority for handle payloads.
pub(crate) struct ObjectArena {
    state: Mutex<ObjectArenaState>,
    cleanup: HandleCleanupState,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
    #[cfg(test)]
    before_pin_observation: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl ObjectArena {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(ObjectArenaState {
                objects: FxHashMap::default(),
                active_pins: 0,
                active_releases: 0,
                sealed: false,
            }),
            cleanup: HandleCleanupState::new(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
            #[cfg(test)]
            before_pin_observation: Mutex::new(None),
        }
    }

    /// Inserts an object under the registry's unique arena owner. Pending
    /// bindings borrow that registry; published records belong to its table.
    /// Taking the published owner fixes the allocation's address before any
    /// binding pointer is created; moving a stack arena cannot invalidate it.
    ///
    /// # Safety
    ///
    /// The caller must retain the arena at this address through every binding,
    /// pin and final release derived from the result. It must validate that
    /// these capabilities have drained before recovering or dropping the
    /// published allocation. The registry is the sole production caller.
    pub(crate) unsafe fn insert<T: Send + Sync + 'static>(
        arena: &PublishedOwner<Self>,
        id: ObjectId,
        value: T,
    ) -> XllResult<ObjectBinding> {
        let owner: Box<dyn Any + Send + Sync> = Box::new(value);
        let mut cell = Box::new(ObjectCell {
            arena: NonNull::from(arena.as_ref()),
            id,
            owner: Some(owner),
            pointer: NonNull::dangling(),
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
        });
        cell.pointer = NonNull::from_ref(cell.owner.as_ref().unwrap().as_ref()).cast::<()>();
        let cell = PublishedOwner::from_box(cell);

        let mut state = arena.state.lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        if state.objects.contains_key(&id) {
            xlfn_kernel::invariant::fail_stop();
        }
        state.objects.insert(
            id,
            ObjectEntry {
                cell,
                bindings: 1,
                pins: 0,
            },
        );
        let cell_pointer = NonNull::from(state.objects.get(&id).unwrap().cell.as_ref());
        drop(state);
        arena.record(crate::shutdown_trace::ShutdownEvent::AddHandleObject);
        Ok(ObjectBinding { cell: cell_pointer })
    }

    fn duplicate_binding(&self, id: ObjectId, cell: NonNull<ObjectCell>) -> XllResult<()> {
        let mut state = self.state.lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        let entry = state.objects.get_mut(&id).ok_or(XllError::StaleHandle)?;
        if NonNull::from(entry.cell.as_ref()) != cell {
            return Err(XllError::StaleHandle);
        }
        entry.bindings = entry.bindings.checked_add(1).ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;
        Ok(())
    }

    #[cfg(any(feature = "async", test))]
    fn acquire_pin(
        &self,
        id: ObjectId,
        cell: NonNull<ObjectCell>,
    ) -> XllResult<RawObjectLeaseGuard> {
        let mut state = self.state.lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        let active_pins = state.active_pins.checked_add(1).ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;
        let entry = state.objects.get_mut(&id).ok_or(XllError::StaleHandle)?;
        if NonNull::from(entry.cell.as_ref()) != cell {
            return Err(XllError::StaleHandle);
        }
        entry.pins = entry.pins.checked_add(1).ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;
        state.active_pins = active_pins;
        drop(state);
        self.record(crate::shutdown_trace::ShutdownEvent::AddHandlePin);
        Ok(RawObjectLeaseGuard { cell })
    }

    fn release_binding(&self, id: ObjectId) {
        // Declare the completion guard outside the lock's scope so even an
        // unwinding transition unlocks before the guard takes this mutex.
        let _release;
        let retired = {
            let mut state = self.state.lock();
            let entry = state
                .objects
                .get_mut(&id)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            entry.bindings = entry
                .bindings
                .checked_sub(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            if entry.bindings != 0 || entry.pins != 0 {
                // No application destruction or observation follows this
                // transition. Unlocking is the final arena access, so the
                // release finishes without a separate completion tail.
                return;
            }
            _release = self.begin_release(&mut state);
            state
                .objects
                .remove(&id)
                .expect("object entry was present")
                .cell
        };
        self.destroy(retired);
    }

    fn release_pin(&self, id: ObjectId) {
        self.release_pin_observed::<{ cfg!(any(test, feature = "refinement")) }>(id);
    }

    /// Trace-enabled builds retain a tail even for a nonfinal pin: recording
    /// the removal can block after the capability-transition lock is dropped.
    /// Unobserved production releases finish under that lock unless they
    /// retire the payload. Tests explicitly exercise both instantiations.
    fn release_pin_observed<const OBSERVED: bool>(&self, id: ObjectId) {
        let _release;
        let retired = {
            let mut state = self.state.lock();
            let reclaim = {
                let entry = state
                    .objects
                    .get_mut(&id)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
                entry.pins = entry
                    .pins
                    .checked_sub(1)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
                entry.bindings == 0 && entry.pins == 0
            };
            state.active_pins = state
                .active_pins
                .checked_sub(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            if !reclaim && !OBSERVED {
                // Neither the cell nor the arena is touched after unlocking.
                // A concurrent final release may reclaim them immediately.
                return;
            }
            _release = self.begin_release(&mut state);
            reclaim.then(|| {
                state
                    .objects
                    .remove(&id)
                    .expect("object entry was present")
                    .cell
            })
        };
        if OBSERVED {
            self.record(crate::shutdown_trace::ShutdownEvent::RemoveHandlePin);
        }
        if let Some(cell) = retired {
            self.destroy(cell);
        }
    }

    fn destroy(&self, cell: PublishedOwner<ObjectCell>) {
        // Binding grace periods and the arena's pin count have both drained.
        let mut cell = cell.into_box();
        let owner = cell
            .owner
            .take()
            .expect("object cell owner is consumed exactly once");
        if catch_no_unwind(AssertUnwindSafe(|| drop(owner))).is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle object final drop", &error);
            self.cleanup.record(error);
        }
        drop(cell);
        self.record(crate::shutdown_trace::ShutdownEvent::RemoveHandleObject);
    }

    /// Keep final-release completion distinct from the payload counts: user
    /// destruction can reenter the registry after the last payload is removed.
    /// The arena must still exist for diagnostics and the final notification.
    ///
    /// Register completion while the caller holds the same lock that removes
    /// its capability. This closes the payload-to-release accounting handoff
    /// without acquiring the arena mutex a separate time.
    fn begin_release<'arena>(
        &'arena self,
        state: &mut ObjectArenaState,
    ) -> impl Drop + 'arena + use<'arena> {
        state.active_releases = state
            .active_releases
            .checked_add(1)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        scopeguard::guard(self, |arena| {
            let mut state = arena.state.lock();
            state.active_releases = state
                .active_releases
                .checked_sub(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        })
    }

    pub(crate) fn seal(&self) {
        self.state.lock().sealed = true;
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup.result()
    }

    pub(crate) fn finish_quiescence(&self) -> XllResult<()> {
        let state = self.state.lock();
        if state.active_pins != 0 {
            // A public `HandleLease` cannot outlive its generated async task.
            // Reaching this branch after async shutdown is therefore a
            // framework ordering violation, retained as a diagnostic so the
            // existing close certificate reports the failed invariant.
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_PINS,
            });
        }
        if !state.objects.is_empty() || state.active_releases != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_OBJECTS,
            });
        }
        drop(state);
        self.cleanup.result()
    }

    /// Validate while allocation access is still shared, before its unique
    /// owner recovers a Box or starts destruction. Checking only inside Drop
    /// would happen after Box reconstruction has already claimed exclusivity.
    pub(crate) fn assert_reclaimable(&self) {
        let state = self.state.lock();
        if !state.objects.is_empty() || state.active_pins != 0 || state.active_releases != 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(trace);
    }

    fn record(&self, event: crate::shutdown_trace::ShutdownEvent) {
        #[cfg(test)]
        if matches!(event, crate::shutdown_trace::ShutdownEvent::RemoveHandlePin) {
            // The fixture can block or reenter exactly where a trace recorder
            // would run, after releasing the arena transition mutex.
            let hook = self.before_pin_observation.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        #[cfg(any(test, feature = "refinement"))]
        if let Some(trace) = self.trace.get() {
            trace.record(event);
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = event;
    }
}

impl Drop for ObjectArena {
    fn drop(&mut self) {
        let state = self.state.lock();
        if !state.objects.is_empty() || state.active_pins != 0 || state.active_releases != 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

pub(crate) struct ObjectCell {
    arena: NonNull<ObjectArena>,
    id: ObjectId,
    owner: Option<Box<dyn Any + Send + Sync>>,
    pointer: NonNull<()>,
    type_id: TypeId,
    type_name: &'static str,
}

impl ObjectCell {
    pub(crate) fn id(&self) -> ObjectId {
        self.id
    }

    pub(crate) fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub(crate) fn type_name(&self) -> &'static str {
        self.type_name
    }

    pub(crate) fn typed_projection<T: 'static>(&self) -> Option<TypedObjectProjection<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| TypedObjectProjection {
            pointer: self.pointer.cast(),
        })
    }
}

// SAFETY: ObjectCell is immutable once published and safe to transfer across threads.
unsafe impl Send for ObjectCell {}
// SAFETY: ObjectCell contents are immutable and safe to share across threads.
unsafe impl Sync for ObjectCell {}

/// One counted binding; its registry owns the arena for the entire capability.
#[repr(transparent)]
pub(crate) struct ObjectBinding {
    cell: NonNull<ObjectCell>,
}

impl ObjectBinding {
    #[inline]
    pub(crate) fn arena(&self) -> NonNull<ObjectArena> {
        self.object().arena
    }

    pub(crate) fn id(&self) -> ObjectId {
        self.object().id
    }

    pub(crate) fn object(&self) -> &ObjectCell {
        // SAFETY: every binding contributes one arena count and therefore
        // prevents object reclamation.
        unsafe { self.cell.as_ref() }
    }

    pub(crate) fn duplicate(&self) -> XllResult<Self> {
        // SAFETY: the binding count prevents the arena owner from completing
        // reclamation; every production binding is nested inside its registry.
        unsafe { self.arena().as_ref() }.duplicate_binding(self.id(), self.cell)?;
        Ok(Self { cell: self.cell })
    }

    #[cfg(any(feature = "async", test))]
    pub(crate) fn acquire_lease(&self) -> XllResult<RawObjectLeaseGuard> {
        // SAFETY: this binding holds an arena count throughout pin admission.
        unsafe { self.arena().as_ref() }.acquire_pin(self.id(), self.cell)
    }
}

/// An un-published object binding capability anchored to the borrow of its
/// owning [`HandleRegistry`].
pub(crate) struct PendingObjectBinding<'registry> {
    binding: ObjectBinding,
    _registry: std::marker::PhantomData<&'registry super::registry::HandleRegistry>,
}

impl<'registry> PendingObjectBinding<'registry> {
    #[inline]
    pub(super) fn new(
        registry: &'registry super::registry::HandleRegistry,
        binding: ObjectBinding,
    ) -> XllResult<Self> {
        if !registry.owns_object_arena(binding.arena()) {
            return Err(XllError::StaleHandle);
        }
        Ok(Self {
            binding,
            _registry: std::marker::PhantomData,
        })
    }

    /// Publishes this capability directly into its registry owner.
    ///
    /// The lifetime anchor is consumed together with the binding; no
    /// lifetime-less `ObjectBinding` is returned to the caller.
    #[inline]
    pub(super) fn publish<T: Send + Sync + 'static>(
        self,
        registry: &'registry super::registry::HandleRegistry,
    ) -> XllResult<(String, super::token::HandleId, ObjectId, bool)> {
        let mut binding = Some(self.binding);
        registry.insert_pending_object_with_kind::<T>(&mut binding)
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn id(&self) -> ObjectId {
        self.binding.id()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Test accessor fixture")]
    #[inline]
    pub(crate) fn object(&self) -> &ObjectCell {
        self.binding.object()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Test accessor fixture")]
    #[inline]
    pub(crate) fn arena(&self) -> NonNull<ObjectArena> {
        self.binding.arena()
    }
}

impl Drop for ObjectBinding {
    fn drop(&mut self) {
        // SAFETY: retirement passed its grace period while borrowing the
        // registry owner. Every binding owns one count until Drop; the arena
        // checks final-release completion before allowing reclamation.
        let (arena, id) = {
            let cell = self.object();
            (cell.arena, cell.id)
        };
        // SAFETY: capture metadata before release can destroy the cell. The
        // registry retains the arena through final-release completion.
        unsafe { arena.as_ref() }.release_binding(id);
    }
}

// SAFETY: ObjectBinding is a non-owning thread-safe handle to an arena-managed object.
unsafe impl Send for ObjectBinding {}
// SAFETY: ObjectBinding immutable borrows can be shared across threads.
unsafe impl Sync for ObjectBinding {}

/// Internal pin capability held by a generated async handle task.
///
/// Generated tasks retain their execution lease through this pin's final
/// release; task drain precedes service teardown and arena quiescence checks.
#[repr(transparent)]
pub(crate) struct RawObjectLeaseGuard {
    cell: NonNull<ObjectCell>,
}

impl Drop for RawObjectLeaseGuard {
    fn drop(&mut self) {
        // SAFETY: every guard owns one pin until Drop. Async execution and the
        // checked pin count keep the arena alive until this release.
        let (arena, id) = {
            // SAFETY: this guard retains one pin, keeping the cell alive.
            let cell = unsafe { self.cell.as_ref() };
            (cell.arena, cell.id)
        };
        // SAFETY: metadata was captured before the final release; async drain
        // retains the arena through completion. Do not access the cell again.
        unsafe { arena.as_ref() }.release_pin(id);
    }
}

// SAFETY: RawObjectLeaseGuard holds a pin count in a thread-safe arena and can be transferred.
unsafe impl Send for RawObjectLeaseGuard {}
// SAFETY: RawObjectLeaseGuard immutable borrows can be shared across threads.
unsafe impl Sync for RawObjectLeaseGuard {}

#[cfg(all(feature = "bench-internals", feature = "async"))]
pub use benchmark::{ObjectFinalPinRelease, ObjectLeaseBenchCase, ObjectLeaseBenchmark};

#[cfg(all(feature = "bench-internals", feature = "async"))]
mod benchmark {
    use super::*;
    use std::hint::black_box;
    use std::sync::{Arc, Barrier, mpsc};

    /// The object distribution for a shared-arena pin workload.
    #[derive(Clone, Copy)]
    pub enum ObjectLeaseBenchCase {
        SameObject,
        DistinctObjects,
    }

    impl ObjectLeaseBenchCase {
        pub const ALL: [Self; 2] = [Self::SameObject, Self::DistinctObjects];

        pub const fn name(self) -> &'static str {
            match self {
                Self::SameObject => "same_object",
                Self::DistinctObjects => "distinct_objects",
            }
        }
    }

    /// Persistent workers exercising production pin admission and release.
    /// Threads, bindings, allocations, and channels are prepared before timing.
    pub struct ObjectLeaseBenchmark {
        binding: Option<ObjectBinding>,
        arena: Arc<PublishedOwner<ObjectArena>>,
        iterations_per_worker: usize,
        start_tx: Vec<mpsc::SyncSender<bool>>,
        done_rx: Vec<mpsc::Receiver<()>>,
        barrier: Arc<Barrier>,
        workers: Vec<std::thread::JoinHandle<()>>,
    }

    impl ObjectLeaseBenchmark {
        pub fn new(
            case: ObjectLeaseBenchCase,
            worker_count: usize,
            iterations_per_worker: usize,
        ) -> Self {
            assert!(worker_count != 0);
            assert!(iterations_per_worker != 0);
            let arena = Arc::new(PublishedOwner::new(ObjectArena::new()));
            // SAFETY: this benchmark retains the published arena owner until
            // all worker bindings, pins, and release completions have drained.
            let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), 42_u64) }
                .expect("benchmark seed binding");
            let mut start_tx = Vec::with_capacity(worker_count);
            let mut done_rx = Vec::with_capacity(worker_count);
            let mut workers = Vec::with_capacity(worker_count);
            let barrier = Arc::new(Barrier::new(worker_count + 1));
            for worker in 0..worker_count {
                let worker_binding = match case {
                    ObjectLeaseBenchCase::SameObject => {
                        binding.duplicate().expect("benchmark duplicate binding")
                    }
                    ObjectLeaseBenchCase::DistinctObjects => {
                        let id = ObjectId::new(
                            1,
                            u64::try_from(worker + 2).expect("worker object ID fits u64"),
                        );
                        // SAFETY: every worker retains an arena owner through
                        // explicit binding destruction and its release tail.
                        unsafe { ObjectArena::insert(&arena, id, 42_u64) }
                            .expect("benchmark worker binding")
                    }
                };
                let worker_arena = Arc::clone(&arena);
                let worker_barrier = Arc::clone(&barrier);
                let (start, receive) = mpsc::sync_channel::<bool>(1);
                start_tx.push(start);
                let (done, completed) = mpsc::sync_channel(1);
                done_rx.push(completed);
                workers.push(std::thread::spawn(move || {
                    while let Ok(pin_cycles) = receive.recv() {
                        // Every worker starts the batch together. Sequential
                        // channel dispatch must not turn the one-worker case
                        // into a staggered, less contended concurrency curve.
                        worker_barrier.wait();
                        if pin_cycles {
                            for _ in 0..iterations_per_worker {
                                let pin = worker_binding.acquire_lease().expect("benchmark pin");
                                drop(black_box(pin));
                            }
                        } else {
                            for _ in 0..iterations_per_worker {
                                black_box(42_u64);
                            }
                        }
                        done.send(()).expect("benchmark driver receives completion");
                    }
                    drop(worker_binding);
                    drop(worker_arena);
                }));
            }
            Self {
                binding: Some(binding),
                arena,
                iterations_per_worker,
                start_tx,
                done_rx,
                barrier,
                workers,
            }
        }

        /// One uncontended pin cycle without worker dispatch.
        pub fn run_serial(&self) {
            let pin = self
                .binding
                .as_ref()
                .expect("benchmark binding is live")
                .acquire_lease()
                .expect("benchmark pin");
            drop(black_box(pin));
        }

        pub fn run(&self) {
            self.run_batch(true);
        }

        /// Same workers, dispatch, start barrier, loop count, and completion
        /// channels without arena operations. A control, not a subtractable
        /// estimate of lock overhead.
        pub fn run_dispatch_control(&self) {
            self.run_batch(false);
        }

        fn run_batch(&self, pin_cycles: bool) {
            for start in &self.start_tx {
                start
                    .send(pin_cycles)
                    .expect("benchmark worker receives start");
            }
            self.barrier.wait();
            for done in &self.done_rx {
                done.recv().expect("benchmark worker completed batch");
            }
        }

        pub fn total_iterations(&self) -> usize {
            self.start_tx.len() * self.iterations_per_worker
        }
    }

    impl Drop for ObjectLeaseBenchmark {
        fn drop(&mut self) {
            self.start_tx.clear();
            for worker in self.workers.drain(..) {
                // A failed worker disconnects its own completion channel, so
                // the driver reports failure without blocking on other live
                // senders. Cleanup must not panic again while unwinding it.
                let _ = crate::panic_boundary::contain_panic(worker.join());
            }
            drop(self.binding.take());
            self.arena.assert_reclaimable();
        }
    }

    /// Setup for an isolated final-pin drop. Criterion prepares this outside
    /// timing; `release` returns the arena owner for untimed output cleanup.
    pub struct ObjectFinalPinRelease {
        pin: Option<RawObjectLeaseGuard>,
        _arena: PublishedOwner<ObjectArena>,
    }

    impl ObjectFinalPinRelease {
        pub fn prepare() -> Self {
            let arena = PublishedOwner::new(ObjectArena::new());
            // SAFETY: this fixture retains the arena through the pin's final
            // release, including application destruction and completion.
            let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), 42_u64) }
                .expect("benchmark final-pin binding");
            let pin = binding.acquire_lease().expect("benchmark final pin");
            drop(binding);
            Self {
                pin: Some(pin),
                _arena: arena,
            }
        }

        pub fn release(mut self) -> Self {
            drop(black_box(
                self.pin.take().expect("benchmark pin releases once"),
            ));
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transfer this guard's counted release to the unobserved instantiation
    /// used by production builds without refinement. ManuallyDrop suppresses
    /// only the now-transferred release; no capability or allocation is leaked.
    fn release_unobserved(pin: RawObjectLeaseGuard) {
        let pin = std::mem::ManuallyDrop::new(pin);
        let (arena, id) = {
            // SAFETY: the still-counted pin keeps this cell and metadata live.
            let cell = unsafe { pin.cell.as_ref() };
            (cell.arena, cell.id)
        };
        // SAFETY: this consumes the pin's one count. No cell/arena access
        // follows the release, including when it retires the last payload.
        unsafe { arena.as_ref() }.release_pin_observed::<false>(id);
    }

    #[test]
    fn miri_unobserved_nonfinal_pin_release_finishes_without_observation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let arena = PublishedOwner::new(ObjectArena::new());
        // SAFETY: this fixture retains the arena through all release tails.
        let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), 42_u32) }.unwrap();
        let duplicate = binding.duplicate().unwrap();
        let first = binding.acquire_lease().unwrap();
        let second = binding.acquire_lease().unwrap();
        let observations = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&observations);
        *arena.before_pin_observation.lock() = Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));

        // The production no-trace branch is explicitly selected, rather than
        // relying on libtest's trace-enabled RawObjectLeaseGuard::drop.
        release_unobserved(first);
        drop(duplicate);
        drop(binding);
        {
            let state = arena.state.lock();
            let entry = &state.objects[&ObjectId::new(1, 1)];
            assert_eq!((entry.bindings, entry.pins), (0, 1));
            assert_eq!((state.active_pins, state.active_releases), (1, 0));
        }
        assert_eq!(observations.load(Ordering::Relaxed), 0);
        release_unobserved(second);
        assert_eq!(observations.load(Ordering::Relaxed), 0);
        arena.finish_quiescence().unwrap();
        arena.assert_reclaimable();
    }

    #[test]
    fn miri_unobserved_pin_and_binding_final_releases_can_race_reclamation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(self.0.fetch_add(1, Ordering::Relaxed), 0);
            }
        }

        for _ in 0..8 {
            let arena = PublishedOwner::new(ObjectArena::new());
            let drops = Arc::new(AtomicUsize::new(0));
            // SAFETY: the owner below waits for arena quiescence before
            // reclaiming it, including any final destruction tail.
            let binding = unsafe {
                ObjectArena::insert(&arena, ObjectId::new(1, 1), DropProbe(Arc::clone(&drops)))
            }
            .unwrap();
            let pin = binding.acquire_lease().unwrap();
            let start = Arc::new(Barrier::new(3));
            let binding_start = Arc::clone(&start);
            let binding_thread = std::thread::spawn(move || {
                binding_start.wait();
                drop(binding);
            });
            let pin_start = Arc::clone(&start);
            let pin_thread = std::thread::spawn(move || {
                pin_start.wait();
                release_unobserved(pin);
            });
            start.wait();
            let deadline = Instant::now() + Duration::from_secs(5);
            while arena.finish_quiescence().is_err() {
                assert!(Instant::now() < deadline, "object release did not finish");
                std::thread::yield_now();
            }
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            arena.assert_reclaimable();
            // Reclamation precedes joining either releaser: a nonfinal fast
            // release must have no arena access after its transition unlocks.
            drop(arena);
            binding_thread.join().unwrap();
            pin_thread.join().unwrap();
        }
    }

    #[test]
    fn miri_observed_nonfinal_pin_retains_tail_after_concurrent_final_release() {
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        struct ArenaPointer(NonNull<ObjectArena>);
        impl ArenaPointer {
            fn get(&self) -> &ObjectArena {
                // SAFETY: this fixture retains the published owner until
                // the observation and all release tails have completed.
                unsafe { self.0.as_ref() }
            }
        }
        // SAFETY: immutable access uses the arena mutex; the observation's
        // counted tail retains the arena through this pointer's final use.
        unsafe impl Send for ArenaPointer {}
        // SAFETY: the same shared-access and release-tail guarantee applies.
        unsafe impl Sync for ArenaPointer {}

        let arena = PublishedOwner::new(ObjectArena::new());
        let trace = triomphe::Arc::new(crate::shutdown_trace::ShutdownTraceRecorder::new());
        trace
            .begin(1, crate::shutdown_trace::ShutdownResources::opened(0, 0))
            .unwrap();
        arena.set_trace_sink(triomphe::Arc::clone(&trace));
        // SAFETY: the owner remains published through all capabilities/tails.
        let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), 42_u32) }.unwrap();
        let first = binding.acquire_lease().unwrap();
        let last = binding.acquire_lease().unwrap();
        drop(binding);
        let pointer = ArenaPointer(NonNull::from(arena.as_ref()));
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let hook_entered = Arc::clone(&entered);
        let hook_resume = Arc::clone(&resume);
        *arena.before_pin_observation.lock() = Some(Arc::new(move || {
            // Lock reentry witnesses that observation is outside the arena
            // transition mutex, with its independent tail already registered.
            {
                let state = pointer.get().state.lock();
                assert_eq!((state.active_pins, state.active_releases), (1, 1));
            }
            hook_entered.wait();
            hook_resume.wait();
        }));
        let first_thread = std::thread::spawn(move || drop(first));
        entered.wait();
        *arena.before_pin_observation.lock() = None;
        drop(last);
        {
            let state = arena.state.lock();
            assert!(state.objects.is_empty());
            assert_eq!((state.active_pins, state.active_releases), (0, 1));
        }
        assert!(matches!(
            arena.finish_quiescence(),
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_OBJECTS,
            })
        ));
        arena.seal();
        resume.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while arena.finish_quiescence().is_err() {
            assert!(Instant::now() < deadline, "observation tail did not finish");
            std::thread::yield_now();
        }
        arena.assert_reclaimable();
        drop(arena);
        first_thread.join().unwrap();
        let activities = trace.activities();
        assert_eq!(
            activities
                .iter()
                .filter(|event| matches!(
                    event,
                    crate::shutdown_trace::ActivityEvent::RemoveHandlePin
                ))
                .count(),
            2,
        );
    }

    #[test]
    fn capability_layout() {
        assert_eq!(size_of::<ObjectBinding>(), size_of::<NonNull<ObjectCell>>(),);
        assert_eq!(
            size_of::<RawObjectLeaseGuard>(),
            size_of::<NonNull<ObjectCell>>(),
        );
        assert_eq!(
            size_of::<ObjectEntry>(),
            size_of::<(NonNull<ObjectCell>, u32, u32)>()
        );
        #[cfg(target_pointer_width = "64")]
        assert_eq!(size_of::<super::super::binding::BindingRecord>(), 32);
        eprintln!(
            "ObjectBinding={} RawObjectLeaseGuard={} BindingRecord={} ObjectCell={} ObjectEntry={}",
            size_of::<ObjectBinding>(),
            size_of::<RawObjectLeaseGuard>(),
            size_of::<super::super::binding::BindingRecord>(),
            size_of::<ObjectCell>(),
            size_of::<ObjectEntry>(),
        );
    }

    #[test]
    fn miri_bindings_and_pins_release_once_in_either_order() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(self.0.fetch_add(1, Ordering::SeqCst), 0);
            }
        }

        for pin_last in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let arena = PublishedOwner::new(ObjectArena::new());
            let id = ObjectId::new(1, 1);
            // SAFETY: arena outlives every binding and pin in this fixture.
            let binding =
                unsafe { ObjectArena::insert(&arena, id, DropProbe(Arc::clone(&drops))) }.unwrap();
            let duplicate = binding.duplicate().unwrap();
            let pin = binding.acquire_lease().unwrap();
            let other_pin = duplicate.acquire_lease().unwrap();
            {
                let state = arena.state.lock();
                let entry = &state.objects[&id];
                assert_eq!(
                    (
                        entry.bindings,
                        entry.pins,
                        state.active_pins,
                        state.active_releases,
                    ),
                    (2, 2, 2, 0),
                );
            }
            // Move capabilities through Option to exercise ownership transfer
            // without any extra release or disarming state.
            let mut binding = Some(binding);
            drop(binding.take());
            drop(other_pin);
            {
                let state = arena.state.lock();
                let entry = &state.objects[&id];
                assert_eq!(
                    (
                        entry.bindings,
                        entry.pins,
                        state.active_pins,
                        state.active_releases,
                    ),
                    (1, 1, 1, 0),
                );
            }
            if pin_last {
                drop(duplicate);
                assert_eq!(drops.load(Ordering::SeqCst), 0);
                drop(pin);
            } else {
                drop(pin);
                assert_eq!(drops.load(Ordering::SeqCst), 0);
                drop(duplicate);
            }
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            let state = arena.state.lock();
            assert!(state.objects.is_empty());
            assert_eq!((state.active_pins, state.active_releases), (0, 0));
        }
    }

    #[test]
    fn miri_final_release_retains_arena_through_reentrant_and_panicking_drop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ReleaseObserver {
            arena: NonNull<ObjectArena>,
            id: ObjectId,
            release_depth: usize,
            drops: Arc<AtomicUsize>,
            nested: Option<ObjectBinding>,
            panic: bool,
        }
        // SAFETY: this fixture's published arena outlives the observing
        // payload, and every access uses the arena's shared mutex.
        unsafe impl Send for ReleaseObserver {}
        // SAFETY: the raw pointer is immutable; the observing payload only
        // accesses the pointed-to arena during its exclusive destruction.
        unsafe impl Sync for ReleaseObserver {}
        impl Drop for ReleaseObserver {
            fn drop(&mut self) {
                // SAFETY: the test retains the unique published arena owner
                // until this destructor and its release guard have finished.
                let arena = unsafe { self.arena.as_ref() };
                {
                    // Lock reentry proves application destruction happens
                    // after the capability-transition lock was released.
                    let state = arena.state.lock();
                    assert!(!state.objects.contains_key(&self.id));
                    assert_eq!(state.active_pins, 0);
                    assert_eq!(state.active_releases, self.release_depth);
                }
                assert!(matches!(
                    arena.finish_quiescence(),
                    Err(XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_OBJECTS,
                    })
                ));
                arena.seal();
                drop(self.nested.take());
                {
                    let state = arena.state.lock();
                    assert_eq!(state.active_releases, self.release_depth);
                    assert!(state.objects.is_empty());
                }
                assert_eq!(self.drops.fetch_add(1, Ordering::SeqCst), 0);
                if self.panic {
                    panic!("application destructor panic");
                }
            }
        }

        for (observed, pin_last, panic) in [false, true].into_iter().flat_map(|observed| {
            [false, true].into_iter().flat_map(move |pin_last| {
                [false, true]
                    .into_iter()
                    .map(move |panic| (observed, pin_last, panic))
            })
        }) {
            let arena = PublishedOwner::new(ObjectArena::new());
            let outer_drops = Arc::new(AtomicUsize::new(0));
            let nested_drops = Arc::new(AtomicUsize::new(0));
            let arena_pointer = NonNull::from(arena.as_ref());
            let nested_id = ObjectId::new(1, 2);
            // SAFETY: all fixture capabilities drain before arena drop.
            let nested = unsafe {
                ObjectArena::insert(
                    &arena,
                    nested_id,
                    ReleaseObserver {
                        arena: arena_pointer,
                        id: nested_id,
                        release_depth: 2,
                        drops: Arc::clone(&nested_drops),
                        nested: None,
                        panic: false,
                    },
                )
            }
            .unwrap();
            let outer_id = ObjectId::new(1, 1);
            // SAFETY: the observing payload borrows the stable arena
            // allocation only while this fixture retains its owner.
            let binding = unsafe {
                ObjectArena::insert(
                    &arena,
                    outer_id,
                    ReleaseObserver {
                        arena: arena_pointer,
                        id: outer_id,
                        release_depth: 1,
                        drops: Arc::clone(&outer_drops),
                        nested: Some(nested),
                        panic,
                    },
                )
            }
            .unwrap();
            let pin = binding.acquire_lease().unwrap();
            if pin_last {
                drop(binding);
                if observed {
                    drop(pin);
                } else {
                    release_unobserved(pin);
                }
            } else {
                if observed {
                    drop(pin);
                } else {
                    release_unobserved(pin);
                }
                drop(binding);
            }
            assert_eq!(outer_drops.load(Ordering::SeqCst), 1);
            assert_eq!(nested_drops.load(Ordering::SeqCst), 1);
            {
                let state = arena.state.lock();
                assert!(state.objects.is_empty());
                assert_eq!((state.active_pins, state.active_releases), (0, 0));
            }
            if panic {
                assert!(matches!(arena.finish_quiescence(), Err(XllError::Panic)));
            } else {
                assert!(arena.finish_quiescence().is_ok());
            }
            arena.assert_reclaimable();
        }
    }

    #[test]
    fn failed_pin_admission_preserves_both_counters() {
        let arena = PublishedOwner::new(ObjectArena::new());
        // SAFETY: arena remains published until the binding and all test pins drop.
        let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), 42_u32) }.unwrap();
        for entry_overflow in [false, true] {
            {
                let mut state = arena.state.lock();
                state.active_pins = if entry_overflow { 0 } else { usize::MAX };
                state.objects.get_mut(&binding.id()).unwrap().pins = if entry_overflow {
                    ObjectPinCount::MAX
                } else {
                    0
                };
            }
            let result = arena.acquire_pin(binding.id(), binding.cell);
            let mut state = arena.state.lock();
            let total = state.active_pins;
            let entry = state.objects.get_mut(&binding.id()).unwrap();
            let pins = entry.pins;
            // Restore synthetic counts before assertions/fixture destruction.
            entry.pins = 0;
            state.active_pins = 0;
            drop(state);
            assert!(matches!(
                result,
                Err(XllError::Domain {
                    code: crate::error::DomainErrorCode::Overflow,
                })
            ));
            assert_eq!(total, if entry_overflow { 0 } else { usize::MAX });
            assert_eq!(
                pins,
                if entry_overflow {
                    ObjectPinCount::MAX
                } else {
                    0
                }
            );
        }
        drop(binding);
    }

    #[test]
    fn binding_count_overflow_preserves_existing_capability() {
        let arena = PublishedOwner::new(ObjectArena::new());
        let id = ObjectId::new(1, 1);
        // SAFETY: the arena remains alive until its sole real binding is dropped.
        let binding = unsafe { ObjectArena::insert(&arena, id, 42_u32) }.unwrap();
        arena.state.lock().objects.get_mut(&id).unwrap().bindings = ObjectBindingCount::MAX;
        let result = binding.duplicate();
        let previous = {
            let mut state = arena.state.lock();
            let count = &mut state.objects.get_mut(&id).unwrap().bindings;
            std::mem::replace(count, 1)
        };
        assert_eq!(previous, ObjectBindingCount::MAX);
        assert!(matches!(
            result,
            Err(XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow
            })
        ));
        drop(binding);
        assert!(arena.state.lock().objects.is_empty());
    }
}
