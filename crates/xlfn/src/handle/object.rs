//! Runtime-owned arena for formula-handle payloads.
//!
//! The arena is the unique owner of every [`ObjectCell`]. Bindings and pins
//! carry counted, non-owning capabilities. The registry owns the arena and
//! drains every capability before reclaiming it. Hot lookup uses projections.
//! An object is reclaimed after both counts reach
//! zero, and its application destructor always runs outside the arena lock.

use super::token::ObjectId;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::{Mutex, MutexGuard};
use crate::{XllError, XllResult};
use crossbeam_utils::CachePadded;
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::collections::hash_map::Entry;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use xlfn_kernel::published_owner::PublishedOwner;

/// A type-checked, non-owning projection into an [`ObjectCell`].
pub(crate) struct TypedObjectProjection<T: 'static> {
    pointer: NonNull<T>,
    // The cached TypeId denotes the exact T, even for covariant payloads.
    _type: PhantomData<fn(T) -> T>,
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

const OBJECT_SHARD_COUNT: usize = 32;

struct ObjectEntry {
    cell: ObjectAllocationOwner,
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
    states: [CachePadded<Mutex<ObjectArenaState>>; OBJECT_SHARD_COUNT],
    cleanup: HandleCleanupState,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
    #[cfg(test)]
    before_pin_observation: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl ObjectArena {
    pub(crate) fn new() -> Self {
        Self {
            states: std::array::from_fn(|_| {
                CachePadded::new(Mutex::new(ObjectArenaState {
                    objects: FxHashMap::default(),
                    active_pins: 0,
                    active_releases: 0,
                    sealed: false,
                }))
            }),
            cleanup: HandleCleanupState::new(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
            #[cfg(test)]
            before_pin_observation: Mutex::new(None),
        }
    }

    #[inline]
    fn state(&self, id: ObjectId) -> &Mutex<ObjectArenaState> {
        // Object sequences are monotonic within their owning registry. Keep
        // unrelated objects off the same mutex and its cache line without
        // adding a second shared counter to every lease transition.
        &self.states[(id.sequence() as usize) & (OBJECT_SHARD_COUNT - 1)]
    }

    fn lock_states(&self) -> [MutexGuard<'_, ObjectArenaState>; OBJECT_SHARD_COUNT] {
        // Only cold seal/quiescence paths take multiple locks. A fixed order
        // gives them the same consistent global snapshot as the former
        // single mutex; hot transitions acquire exactly one shard.
        std::array::from_fn(|index| self.states[index].lock())
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
        let cell = ObjectAllocationOwner::new(NonNull::from(arena.as_ref()), id, value);

        let mut state = arena.state(id).lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        let entry = match state.objects.entry(id) {
            Entry::Vacant(entry) => entry.insert(ObjectEntry {
                cell,
                bindings: 1,
                pins: 0,
            }),
            Entry::Occupied(_) => xlfn_kernel::invariant::fail_stop(),
        };
        let cell_pointer = NonNull::from(entry.cell.as_ref());
        drop(state);
        arena.record(crate::shutdown_trace::ShutdownEvent::AddHandleObject);
        Ok(ObjectBinding { cell: cell_pointer })
    }

    fn duplicate_binding(&self, id: ObjectId, cell: NonNull<ObjectCell>) -> XllResult<()> {
        let mut state = self.state(id).lock();
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
        let mut state = self.state(id).lock();
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
            let mut state = self.state(id).lock();
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
            _release = self.begin_release(id, &mut state);
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
            let mut state = self.state(id).lock();
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
            _release = self.begin_release(id, &mut state);
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

    fn destroy(&self, cell: ObjectAllocationOwner) {
        // Binding grace periods and the arena's pin count have both drained.
        // The specialized owner recovers the exact allocation layout, whose
        // Box cleanup frees storage even when user destruction unwinds.
        if catch_no_unwind(AssertUnwindSafe(|| drop(cell))).is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle object final drop", &error);
            self.cleanup.record(error);
        }
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
        id: ObjectId,
        state: &mut ObjectArenaState,
    ) -> impl Drop + 'arena + use<'arena> {
        state.active_releases = state
            .active_releases
            .checked_add(1)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        scopeguard::guard(self.state(id), |shard| {
            let mut state = shard.lock();
            state.active_releases = state
                .active_releases
                .checked_sub(1)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        })
    }

    pub(crate) fn seal(&self) {
        let mut states = self.lock_states();
        for state in &mut states {
            state.sealed = true;
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup.result()
    }

    pub(crate) fn finish_quiescence(&self) -> XllResult<()> {
        let states = self.lock_states();
        if states.iter().any(|state| state.active_pins != 0) {
            // A public `HandleLease` cannot outlive its generated async task.
            // Reaching this branch after async shutdown is therefore a
            // framework ordering violation, retained as a diagnostic so the
            // existing close certificate reports the failed invariant.
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_PINS,
            });
        }
        if states
            .iter()
            .any(|state| !state.objects.is_empty() || state.active_releases != 0)
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_OBJECTS,
            });
        }
        drop(states);
        self.cleanup.result()
    }

    /// Validate while allocation access is still shared, before its unique
    /// owner recovers a Box or starts destruction. Checking only inside Drop
    /// would happen after Box reconstruction has already claimed exclusivity.
    pub(crate) fn assert_reclaimable(&self) {
        let states = self.lock_states();
        if states.iter().any(|state| {
            !state.objects.is_empty() || state.active_pins != 0 || state.active_releases != 0
        }) {
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
        self.assert_reclaimable();
    }
}

/// Keeps concrete type names available through the vtable for diagnostics,
/// without storing a name alongside every object. Lookup uses the cached TypeId.
trait ErasedObject: Any + Send + Sync {
    fn concrete_type_name(&self) -> &'static str;
    fn allocation_destructor(&self) -> unsafe fn(NonNull<ObjectCell>);
}

impl<T: Any + Send + Sync> ErasedObject for T {
    fn concrete_type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn allocation_destructor(&self) -> unsafe fn(NonNull<ObjectCell>) {
        if combine_object_allocation::<T>() {
            destroy_combined::<T>
        } else {
            destroy_separate::<T>
        }
    }
}

pub(crate) struct ObjectCell {
    arena: NonNull<ObjectArena>,
    id: ObjectId,
    payload: NonNull<dyn ErasedObject>,
    type_id: TypeId,
}

/// The fixed header stays at offset zero; bindings never carry tail metadata.
#[repr(C)]
struct ObjectAllocation<T> {
    cell: ObjectCell,
    payload: T,
}

const fn combine_object_allocation<T>() -> bool {
    // Coallocate only when it removes an allocation without introducing any
    // header/tail alignment padding. Zero-sized Box payloads allocate nothing.
    size_of::<T>() != 0
        && match size_of::<ObjectCell>().checked_add(size_of::<T>()) {
            Some(size) => size == size_of::<ObjectAllocation<T>>(),
            None => false,
        }
}

/// Unique erased allocation owner. Unlike a Box, moving this owner cannot
/// retag the published header or payload. Destruction dispatch is shared in the
/// existing payload vtable rather than retained per object or per binding.
struct ObjectAllocationOwner {
    cell: NonNull<ObjectCell>,
}

impl ObjectAllocationOwner {
    fn new<T: Send + Sync + 'static>(arena: NonNull<ObjectArena>, id: ObjectId, value: T) -> Self {
        if !combine_object_allocation::<T>() {
            return Self::separate(arena, id, value);
        }
        let allocation = PublishedOwner::new(ObjectAllocation {
            cell: ObjectCell {
                arena,
                id,
                payload: NonNull::<T>::dangling(),
                type_id: TypeId::of::<T>(),
            },
            payload: value,
        });
        let pointer = allocation.as_ptr();
        // SAFETY: PublishedOwner has consumed the Box before either pointer
        // is published. Raw field projections retain its original provenance;
        // initialization touches only the header, not the payload projection.
        let payload_pointer = unsafe { &raw mut (*pointer).payload };
        let payload: NonNull<dyn ErasedObject> =
            NonNull::new(payload_pointer).expect("allocated payload pointer is nonnull");
        // SAFETY: this unpublished header is uniquely owned by allocation.
        let payload_slot = unsafe { &raw mut (*pointer).cell.payload };
        // SAFETY: the raw slot points to the live, initialized header field;
        // replacing the initial non-owning pointer cannot drop any payload.
        unsafe { payload_slot.write(payload) };
        // repr(C) places cell at offset zero, preserving allocation provenance.
        let cell = NonNull::new(pointer.cast::<ObjectCell>()).expect("allocated header is nonnull");
        // Transfer the unique allocation obligation to the matching concrete
        // destructor without ever recovering or moving a published Box.
        let _allocation = std::mem::ManuallyDrop::new(allocation);
        Self { cell }
    }

    fn separate<T: Send + Sync + 'static>(
        arena: NonNull<ObjectArena>,
        id: ObjectId,
        value: T,
    ) -> Self {
        let payload = PublishedOwner::new(value);
        let pointer = NonNull::new(payload.as_ptr()).expect("published owner is nonnull");
        let cell = PublishedOwner::new(ObjectCell {
            arena,
            id,
            payload: pointer,
            type_id: TypeId::of::<T>(),
        });
        let pointer = NonNull::new(cell.as_ptr()).expect("published owner is nonnull");
        let _payload = std::mem::ManuallyDrop::new(payload);
        let _cell = std::mem::ManuallyDrop::new(cell);
        Self { cell: pointer }
    }

    fn as_ref(&self) -> &ObjectCell {
        // SAFETY: the unique owner keeps this header and its payload alive.
        unsafe { self.cell.as_ref() }
    }
}

unsafe fn destroy_combined<T>(cell: NonNull<ObjectCell>) {
    // SAFETY: ObjectAllocation<T> is repr(C) with cell at offset zero. The
    // owner was constructed for this exact T, and all readers have drained.
    unsafe { drop(Box::from_non_null(cell.cast::<ObjectAllocation<T>>())) };
}

unsafe fn destroy_separate<T>(cell: NonNull<ObjectCell>) {
    // SAFETY: the separate constructor transferred these exact Box allocations
    // and their unique ownership. All header/payload readers have drained.
    let cell = unsafe { Box::from_non_null(cell) };
    // SAFETY: the constructor paired this concrete payload pointer with T.
    let payload = unsafe { Box::from_non_null(cell.payload.cast::<T>()) };
    // cell remains the allocation guard if payload destruction panics.
    drop(payload);
    drop(cell);
}

impl Drop for ObjectAllocationOwner {
    fn drop(&mut self) {
        // Resolve the concrete destructor while the payload is still live.
        // Copy only pointer/function values out of these borrows: recovering
        // the allocation must never free the receiver of a live vtable call.
        let payload = self.as_ref().payload;
        // SAFETY: this unique owner retains the payload throughout this call.
        let destroy = unsafe { payload.as_ref() }.allocation_destructor();
        // SAFETY: the private blanket implementation selects the same layout
        // as construction. All source borrows above ended before consumption,
        // and the arena has drained every published binding and pin.
        unsafe { destroy(self.cell) };
    }
}

// SAFETY: construction requires Send + Sync payloads and the header is immutable.
unsafe impl Send for ObjectAllocationOwner {}
// SAFETY: shared owner access exposes only the immutable header and payload.
unsafe impl Sync for ObjectAllocationOwner {}

impl ObjectCell {
    pub(crate) fn id(&self) -> ObjectId {
        self.id
    }

    #[cfg(test)]
    pub(crate) fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub(crate) fn type_name(&self) -> &'static str {
        // SAFETY: this header's allocation owner retains the payload.
        unsafe { self.payload.as_ref() }.concrete_type_name()
    }

    pub(crate) fn typed_projection<T: 'static>(&self) -> Option<TypedObjectProjection<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| TypedObjectProjection {
            // Recover the data pointer from the retained owner after checking
            // the cached concrete type. The owner stays present until every
            // binding/read capability and pin has drained.
            pointer: self.payload.cast(),
            _type: PhantomData,
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

/// An unpublished binding retaining its exact payload type and registry borrow.
/// Only allocation of an actual `T` or a checked `HandleAlias<T>` can create it.
/// Type invariance prevents subtyping from changing the cached TypeId authority.
pub(crate) struct PendingObjectBinding<'registry, T: Send + Sync + 'static> {
    binding: ObjectBinding,
    _registry: PhantomData<&'registry super::registry::HandleRegistry>,
    _type: PhantomData<fn(T) -> T>,
}

impl<'registry, T: Send + Sync + 'static> PendingObjectBinding<'registry, T> {
    pub(super) fn new(
        registry: &'registry super::registry::HandleRegistry,
        id: ObjectId,
        value: T,
    ) -> XllResult<Self> {
        // SAFETY: the registry retains the fixed arena allocation. This
        // pending binding borrows it, publication transfers the count into
        // its table, and registry reclamation drains every count and pin.
        let binding = unsafe { ObjectArena::insert(&registry.objects, id, value) }?;
        Ok(Self {
            binding,
            _registry: PhantomData,
            _type: PhantomData,
        })
    }

    pub(super) fn from_alias(
        registry: &'registry super::registry::HandleRegistry,
        alias: super::typed::HandleAlias<'_, T>,
    ) -> XllResult<Self>
    where
        T: super::typed::ExcelHandleObject,
    {
        // HandleAlias<T> can only originate from the checked Handle<T>
        // projection. Its invariant T and live snapshot retain that proof
        // while duplicating the binding count.
        let binding = alias.binding().duplicate_object_binding()?;
        if !registry.owns_object_arena(binding.arena()) {
            return Err(XllError::StaleHandle);
        }
        Ok(Self {
            binding,
            _registry: PhantomData,
            _type: PhantomData,
        })
    }

    /// Publishes the exact typed capability, consuming its registry anchor.
    #[inline]
    pub(super) fn publish(
        self,
        registry: &'registry super::registry::HandleRegistry,
    ) -> XllResult<(String, super::token::HandleId, ObjectId, bool)> {
        // Erase only at the final binding-table insertion. The registry borrow
        // remains live for the entire call; no bare binding escapes this method.
        let mut binding = Some(self.binding);
        registry.insert_pending_object(&mut binding)
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

#[cfg(feature = "bench-internals")]
pub use storage_benchmark::{ObjectPayloadKind, ObjectStorageBenchmark, object_storage_layout};

#[cfg(feature = "bench-internals")]
mod storage_benchmark {
    use super::super::binding::{BindingRecord, BindingTable};
    use super::*;

    /// Nonallocating payload shapes for object storage comparisons.
    #[derive(Clone, Copy, Debug)]
    pub enum ObjectPayloadKind {
        /// A zero-sized payload.
        Zst,
        /// One machine-independent 64-bit scalar.
        U64,
        /// A four-kibibyte inline payload.
        Bytes4096,
        /// A 256-byte-aligned payload with two initialized scalar fields.
        Aligned256,
    }

    impl ObjectPayloadKind {
        /// Every payload shape covered by the fixture.
        pub const ALL: [Self; 4] = [Self::Zst, Self::U64, Self::Bytes4096, Self::Aligned256];

        /// Stable name for benchmark IDs and allocation records.
        pub const fn name(self) -> &'static str {
            match self {
                Self::Zst => "zst",
                Self::U64 => "u64",
                Self::Bytes4096 => "bytes4096",
                Self::Aligned256 => "aligned256",
            }
        }
    }

    #[repr(align(256))]
    struct AlignedPayload([u64; 2]);

    /// Actual arena and binding-table storage, without formula keys, tokens,
    /// producer callbacks or worker startup. Every alias owns a BindingRecord.
    pub struct ObjectStorageBenchmark {
        table: BindingTable,
        arena: PublishedOwner<ObjectArena>,
        objects: usize,
        aliases: usize,
    }

    impl ObjectStorageBenchmark {
        /// Creates empty storage for a nonzero number of objects and aliases.
        pub fn new(objects: usize, aliases: usize) -> Self {
            assert!(objects > 0 && aliases > 0);
            let maximum = objects.checked_mul(aliases).expect("bounded fixture count");
            Self {
                table: BindingTable::new(u32::try_from(maximum).expect("fixture fits u32")),
                arena: PublishedOwner::new(ObjectArena::new()),
                objects,
                aliases,
            }
        }

        /// Populates an empty fixture; aliases include the original binding.
        pub fn populate(&self, kind: ObjectPayloadKind) {
            for index in 0..self.objects {
                let id = ObjectId::new(1, index as u64 + 1);
                let binding = match kind {
                    ObjectPayloadKind::Zst => self.insert_payload(id, ()),
                    ObjectPayloadKind::U64 => self.insert_payload(id, index as u64),
                    ObjectPayloadKind::Bytes4096 => self.insert_payload(id, [index as u8; 4096]),
                    ObjectPayloadKind::Aligned256 => {
                        let value = AlignedPayload([index as u64; 2]);
                        std::hint::black_box(value.0);
                        self.insert_payload(id, value)
                    }
                };
                for _ in 1..self.aliases {
                    let duplicate = binding.duplicate().unwrap();
                    self.table.reserve().unwrap().publish(duplicate);
                }
                self.table.reserve().unwrap().publish(binding);
            }
        }

        fn insert_payload<T: Send + Sync + 'static>(
            &self,
            id: ObjectId,
            value: T,
        ) -> ObjectBinding {
            // SAFETY: the fixture retains its published arena through table
            // retirement and final payload destruction in clear.
            unsafe { ObjectArena::insert(&self.arena, id, value) }.unwrap()
        }

        /// Withdraws every binding and waits for final payload destruction.
        pub fn clear(&self) {
            let (_, retired) = self.table.retire_all();
            drop(retired);
            self.arena.finish_quiescence().unwrap();
        }

        /// Populates and clears the fixture while retaining reusable storage.
        pub fn run_cycle(&self, kind: ObjectPayloadKind) {
            self.populate(kind);
            self.clear();
        }
    }

    impl Drop for ObjectStorageBenchmark {
        fn drop(&mut self) {
            self.clear();
            self.arena.assert_reclaimable();
        }
    }

    /// Reports retained metadata layouts without claiming allocator usable size.
    pub fn object_storage_layout() -> serde_json::Value {
        serde_json::json!({
            "object_binding": size_of::<ObjectBinding>(),
            "binding_record": size_of::<BindingRecord>(),
            "object_entry": size_of::<ObjectEntry>(),
            "object_arena": size_of::<ObjectArena>(),
            "payload_u64": size_of::<u64>(),
            "payload_aligned256": size_of::<AlignedPayload>(),
        })
    }
}

#[cfg(all(feature = "bench-internals", feature = "async"))]
mod benchmark {
    use super::*;
    use std::hint::black_box;
    use std::sync::{Arc, Barrier, mpsc};

    /// The object distribution for a shared-arena pin workload.
    #[derive(Clone, Copy)]
    pub enum ObjectLeaseBenchCase {
        /// All workers acquire pins on one shared object.
        SameObject,
        /// Each worker acquires pins on a separate object in the same arena.
        DistinctObjects,
    }

    impl ObjectLeaseBenchCase {
        /// Every supported object distribution.
        pub const ALL: [Self; 2] = [Self::SameObject, Self::DistinctObjects];

        /// Stable case label used in benchmark output.
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
        /// Prepares persistent workers and bindings outside the measured region.
        ///
        /// Both the worker count and iterations per worker must be nonzero.
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

        /// Runs one synchronized pin-acquire/release batch on every worker.
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

        /// Returns the total number of pin cycles in one worker batch.
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
        /// Prepares an object retained by its last pin, outside the timed region.
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

        /// Drops the final pin and returns the arena owner for untimed cleanup.
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

    #[test]
    fn distinct_object_pins_do_not_wait_for_an_unrelated_shard() {
        use std::sync::mpsc;
        use std::time::Duration;

        let arena = PublishedOwner::new(ObjectArena::new());
        let first_id = ObjectId::new(1, 1);
        let second_id = ObjectId::new(1, 2);
        // SAFETY: the fixture joins the worker and drains both objects before
        // recovering the arena's published owner.
        let first = unsafe { ObjectArena::insert(&arena, first_id, 1_u32) }.unwrap();
        // SAFETY: the same arena lifetime guarantee covers the second object.
        let second = unsafe { ObjectArena::insert(&arena, second_id, 2_u32) }.unwrap();
        let locked = arena.state(first_id).lock();
        let (completed, received) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let pin = second.acquire_lease().unwrap();
            release_unobserved(pin);
            drop(second);
            completed.send(()).unwrap();
        });
        let outcome = received.recv_timeout(Duration::from_secs(5));
        drop(locked);
        worker.join().unwrap();
        outcome.expect("a different object shard must remain independently usable");
        drop(first);
        arena.finish_quiescence().unwrap();
    }

    #[test]
    fn seal_rejects_new_capabilities_in_every_object_shard() {
        let arena = PublishedOwner::new(ObjectArena::new());
        let bindings: Vec<_> = (0..OBJECT_SHARD_COUNT)
            .map(|sequence| {
                let id = ObjectId::new(1, sequence as u64);
                // SAFETY: all returned bindings are dropped before the arena.
                unsafe { ObjectArena::insert(&arena, id, sequence) }.unwrap()
            })
            .collect();
        let pin = bindings[OBJECT_SHARD_COUNT - 1].acquire_lease().unwrap();
        arena.seal();
        for binding in &bindings {
            assert!(matches!(binding.duplicate(), Err(XllError::Closing)));
            assert!(matches!(binding.acquire_lease(), Err(XllError::Closing)));
        }
        drop(bindings);
        assert!(matches!(
            arena.finish_quiescence(),
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_PINS,
            })
        ));
        release_unobserved(pin);
        arena.finish_quiescence().unwrap();
        arena.assert_reclaimable();
    }

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
            let state = arena.state(ObjectId::new(1, 1)).lock();
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
            .begin(
                1,
                crate::shutdown_trace::ShutdownResources::opened(0, 0, false),
            )
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
                let state = pointer.get().state(ObjectId::new(1, 1)).lock();
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
            let state = arena.state(ObjectId::new(1, 1)).lock();
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
            size_of::<(ObjectAllocationOwner, u32, u32)>()
        );
        #[cfg(target_pointer_width = "64")]
        assert_eq!(size_of::<super::super::binding::BindingRecord>(), 32);
        eprintln!(
            "ObjectBinding={} RawObjectLeaseGuard={} BindingRecord={} ObjectCell={} ObjectEntry={} ObjectArena={} ObjectArenaShard={}",
            size_of::<ObjectBinding>(),
            size_of::<RawObjectLeaseGuard>(),
            size_of::<super::super::binding::BindingRecord>(),
            size_of::<ObjectCell>(),
            size_of::<ObjectEntry>(),
            size_of::<ObjectArena>(),
            size_of::<CachePadded<Mutex<ObjectArenaState>>>(),
        );
    }

    #[test]
    fn miri_typed_projection_survives_shared_borrows_and_binding_retirement() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Payload {
            values: [u64; 4],
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                assert_eq!(self.drops.fetch_add(1, Ordering::Relaxed), 0);
            }
        }

        let arena = PublishedOwner::new(ObjectArena::new());
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: bindings and the final pin drain before arena recovery.
        let binding = unsafe {
            ObjectArena::insert(
                &arena,
                ObjectId::new(1, 1),
                Payload {
                    values: [1, 2, 3, 4],
                    drops: Arc::clone(&drops),
                },
            )
        }
        .unwrap();
        let duplicate = binding.duplicate().unwrap();
        let pin = binding.acquire_lease().unwrap();
        assert_eq!(binding.object().type_name(), type_name::<Payload>());
        assert_eq!(binding.object().type_id(), TypeId::of::<Payload>());
        assert!(binding.object().typed_projection::<[u64; 4]>().is_none());
        let projection = binding.object().typed_projection::<Payload>().unwrap();
        // SAFETY: bindings and then the pin retain the payload across all reads.
        let first = unsafe { projection.as_ref_unchecked() };
        assert_eq!(duplicate.object().type_name(), type_name::<Payload>());
        let second_projection = duplicate.object().typed_projection::<Payload>().unwrap();
        // SAFETY: the same live capabilities protect this second shared borrow.
        let second = unsafe { second_projection.as_ref_unchecked() };
        assert!(std::ptr::eq(first, second));
        drop(binding);
        drop(duplicate);
        assert_eq!(first.values, [1, 2, 3, 4]);
        assert_eq!(second.values, first.values);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(pin);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        arena.finish_quiescence().unwrap();
        arena.assert_reclaimable();
    }

    #[test]
    fn miri_typed_projection_preserves_overaligned_zst_pointer() {
        #[repr(align(256))]
        struct Aligned;

        let arena = PublishedOwner::new(ObjectArena::new());
        // SAFETY: the binding drains before arena recovery.
        let binding = unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), Aligned) }.unwrap();
        let projection = binding.object().typed_projection::<Aligned>().unwrap();
        assert_eq!(binding.object().type_name(), type_name::<Aligned>());
        assert_eq!(projection.addr() % align_of::<Aligned>(), 0);
        // SAFETY: binding retains this zero-sized payload throughout the borrow.
        let _value = unsafe { projection.as_ref_unchecked() };
        assert!(binding.object().typed_projection::<()>().is_none());
        drop(binding);
        arena.finish_quiescence().unwrap();
        arena.assert_reclaimable();
    }

    #[test]
    fn miri_overaligned_tail_survives_moves_aliases_and_panicking_final_drop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FieldDrop(Arc<AtomicUsize>);
        impl Drop for FieldDrop {
            fn drop(&mut self) {
                assert_eq!(self.0.fetch_add(1, Ordering::Relaxed), 0);
            }
        }
        #[repr(align(256))]
        struct Payload {
            values: [u64; 4],
            drops: Arc<AtomicUsize>,
            _field: FieldDrop,
            panic: bool,
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                assert_eq!(self.drops.fetch_add(1, Ordering::Relaxed), 0);
                assert!(!self.panic, "aligned payload destructor panic");
            }
        }

        for panic in [false, true] {
            let arena = PublishedOwner::new(ObjectArena::new());
            let drops = Arc::new(AtomicUsize::new(0));
            let field_drops = Arc::new(AtomicUsize::new(0));
            // SAFETY: the published arena owner survives every alias and pin.
            let binding = unsafe {
                ObjectArena::insert(
                    &arena,
                    ObjectId::new(1, 1),
                    Payload {
                        values: [11, 22, 33, 44],
                        drops: Arc::clone(&drops),
                        _field: FieldDrop(Arc::clone(&field_drops)),
                        panic,
                    },
                )
            }
            .unwrap();
            let projection = binding.object().typed_projection::<Payload>().unwrap();
            assert_eq!(projection.addr() % align_of::<Payload>(), 0);
            assert!(binding.object().typed_projection::<[u64; 4]>().is_none());
            let pin = binding.acquire_lease().unwrap();
            let aliases: Vec<_> = (0..8).map(|_| binding.duplicate().unwrap()).collect();
            // Move the stable arena owner after publishing capabilities, and
            // force its object shard to move existing map entries on growth.
            let mut owners = vec![arena];
            for _ in 0..8 {
                owners.push(PublishedOwner::new(ObjectArena::new()));
            }
            let arena = &owners[0];
            let neighbors: Vec<_> = (1..9)
                .map(|index| {
                    // SAFETY: all neighbors drain before the same arena owner.
                    unsafe {
                        ObjectArena::insert(
                            arena,
                            ObjectId::new(1, 1 + index * OBJECT_SHARD_COUNT as u64),
                            index,
                        )
                    }
                    .unwrap()
                })
                .collect();
            drop(binding);
            drop(aliases);
            drop(neighbors);
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            // SAFETY: the remaining pin retains this aligned tail after every
            // map, owner and alias movement and after all bindings retired.
            let value = unsafe { projection.as_ref_unchecked() };
            assert_eq!(value.values, [11, 22, 33, 44]);
            drop(pin);
            assert_eq!(drops.load(Ordering::Relaxed), 1);
            assert_eq!(field_drops.load(Ordering::Relaxed), 1);
            if panic {
                assert!(matches!(arena.finish_quiescence(), Err(XllError::Panic)));
            } else {
                arena.finish_quiescence().unwrap();
            }
            arena.assert_reclaimable();
        }
    }

    #[test]
    fn miri_rejected_object_insert_destroys_unpublished_payload() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DropProbe(Arc<AtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                assert_eq!(self.0.fetch_add(1, Ordering::Relaxed), 0);
            }
        }
        let arena = PublishedOwner::new(ObjectArena::new());
        arena.seal();
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: the owner remains published through this rejected insertion.
        let result = unsafe {
            ObjectArena::insert(&arena, ObjectId::new(1, 1), DropProbe(Arc::clone(&drops)))
        };
        assert!(matches!(result, Err(XllError::Closing)));
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        arena.finish_quiescence().unwrap();
    }

    #[cfg(feature = "bench-internals")]
    #[test]
    fn miri_object_storage_fixture_keeps_all_aliases_until_clear() {
        for kind in ObjectPayloadKind::ALL {
            let mut fixtures = Vec::new();
            let fixture = ObjectStorageBenchmark::new(2, 4);
            fixture.populate(kind);
            fixtures.push(fixture);
            for _ in 0..8 {
                fixtures.push(ObjectStorageBenchmark::new(1, 1));
            }
            fixtures[0].clear();
            fixtures[0].run_cycle(kind);
        }
    }

    #[test]
    fn miri_erased_object_names_boxed_payload_not_erasure_container() {
        let arena = PublishedOwner::new(ObjectArena::new());
        // SAFETY: both bindings drain before arena recovery.
        let boxed =
            unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 1), Box::new([1_u64, 2, 3])) }
                .unwrap();
        // SAFETY: both bindings drain before arena recovery.
        let string =
            unsafe { ObjectArena::insert(&arena, ObjectId::new(1, 2), String::from("payload")) }
                .unwrap();
        assert_eq!(boxed.object().type_name(), type_name::<Box<[u64; 3]>>());
        assert_eq!(string.object().type_name(), type_name::<String>());
        assert_eq!(boxed.object().type_id(), TypeId::of::<Box<[u64; 3]>>());
        assert!(boxed.object().typed_projection::<[u64; 3]>().is_none());
        let projection = boxed.object().typed_projection::<Box<[u64; 3]>>().unwrap();
        // SAFETY: boxed retains the payload throughout this read.
        assert_eq!(**unsafe { projection.as_ref_unchecked() }, [1, 2, 3]);
        drop(boxed);
        drop(string);
        arena.finish_quiescence().unwrap();
        arena.assert_reclaimable();
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
                let state = arena.state(ObjectId::new(1, 1)).lock();
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
                let state = arena.state(ObjectId::new(1, 1)).lock();
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
            let state = arena.state(ObjectId::new(1, 1)).lock();
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
                    let states = arena.lock_states();
                    assert!(
                        states
                            .iter()
                            .all(|state| !state.objects.contains_key(&self.id))
                    );
                    assert!(states.iter().all(|state| state.active_pins == 0));
                    assert_eq!(
                        states
                            .iter()
                            .map(|state| state.active_releases)
                            .sum::<usize>(),
                        self.release_depth,
                    );
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
                    let states = arena.lock_states();
                    assert_eq!(
                        states
                            .iter()
                            .map(|state| state.active_releases)
                            .sum::<usize>(),
                        self.release_depth,
                    );
                    assert!(states.iter().all(|state| state.objects.is_empty()));
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
                let state = arena.state(ObjectId::new(1, 1)).lock();
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
                let mut state = arena.state(ObjectId::new(1, 1)).lock();
                state.active_pins = if entry_overflow { 0 } else { usize::MAX };
                state.objects.get_mut(&binding.id()).unwrap().pins = if entry_overflow {
                    ObjectPinCount::MAX
                } else {
                    0
                };
            }
            let result = arena.acquire_pin(binding.id(), binding.cell);
            let mut state = arena.state(ObjectId::new(1, 1)).lock();
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
        arena
            .state(ObjectId::new(1, 1))
            .lock()
            .objects
            .get_mut(&id)
            .unwrap()
            .bindings = ObjectBindingCount::MAX;
        let result = binding.duplicate();
        let previous = {
            let mut state = arena.state(ObjectId::new(1, 1)).lock();
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
        assert!(arena.state(ObjectId::new(1, 1)).lock().objects.is_empty());
    }
}
