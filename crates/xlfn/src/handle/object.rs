//! Runtime-owned arena for formula-handle payloads.
//!
//! The arena is the unique owner of every [`ObjectCell`]. Bindings and pins
//! carry counted, non-owning capabilities; neither participates in memory
//! ownership. An object is reclaimed only after both capability counts reach
//! zero, and its application destructor always runs outside the arena lock.

use super::token::ObjectId;
use crate::panic_boundary::catch_no_unwind;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
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

struct ObjectEntry {
    cell: PublishedOwner<ObjectCell>,
    bindings: usize,
    pins: usize,
}

struct ObjectArenaState {
    objects: FxHashMap<ObjectId, ObjectEntry>,
    active_pins: usize,
    sealed: bool,
}

/// Unique owner and reclamation authority for handle payloads.
pub(crate) struct ObjectArena {
    state: Mutex<ObjectArenaState>,
    cleanup: HandleCleanupState,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
}

impl ObjectArena {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(ObjectArenaState {
                objects: FxHashMap::default(),
                active_pins: 0,
                sealed: false,
            }),
            cleanup: HandleCleanupState::new(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
        }
    }

    /// Inserts a new object into the arena and returns an initial binding.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self` (the arena) outlives all returned
    /// [`ObjectBinding`] instances (and any [`RawObjectLeaseGuard`] acquired from them).
    /// The owner must drain or drop all binding capabilities before reclaiming the arena.
    pub(crate) unsafe fn insert<T: Send + Sync + 'static>(
        &self,
        id: ObjectId,
        value: T,
    ) -> XllResult<ObjectBinding> {
        let owner: Box<dyn Any + Send + Sync> = Box::new(value);
        let mut cell = Box::new(ObjectCell {
            id,
            owner: Some(owner),
            pointer: NonNull::dangling(),
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
        });
        cell.pointer = NonNull::from_ref(cell.owner.as_ref().unwrap().as_ref()).cast::<()>();
        let cell = PublishedOwner::from_box(cell);

        let mut state = self.state.lock();
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
        self.record(crate::shutdown_trace::ShutdownEvent::AddHandleObject);
        Ok(ObjectBinding {
            arena: NonNull::from(self),
            cell: cell_pointer,
            id,
            armed: true,
        })
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
        Ok(RawObjectLeaseGuard {
            arena: NonNull::from(self),
            id,
            armed: true,
        })
    }

    fn release_binding(&self, id: ObjectId) {
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
            if entry.bindings == 0 && entry.pins == 0 {
                Some(
                    state
                        .objects
                        .remove(&id)
                        .expect("object entry was present")
                        .cell,
                )
            } else {
                None
            }
        };
        if let Some(cell) = retired {
            self.destroy(cell);
        }
    }

    fn release_pin(&self, id: ObjectId) {
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
            reclaim.then(|| {
                state
                    .objects
                    .remove(&id)
                    .expect("object entry was present")
                    .cell
            })
        };
        self.record(crate::shutdown_trace::ShutdownEvent::RemoveHandlePin);
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
        if !state.objects.is_empty() {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_OBJECTS,
            });
        }
        drop(state);
        self.cleanup.result()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(trace);
    }

    fn record(&self, event: crate::shutdown_trace::ShutdownEvent) {
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
        if !state.objects.is_empty() || state.active_pins != 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

pub(crate) struct ObjectCell {
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

/// One formula binding's non-owning, counted capability to an object.
pub(crate) struct ObjectBinding {
    arena: NonNull<ObjectArena>,
    cell: NonNull<ObjectCell>,
    id: ObjectId,
    armed: bool,
}

impl ObjectBinding {
    #[inline]
    pub(crate) fn arena(&self) -> NonNull<ObjectArena> {
        self.arena
    }

    pub(crate) fn id(&self) -> ObjectId {
        self.id
    }

    pub(crate) fn object(&self) -> &ObjectCell {
        // SAFETY: an armed binding contributes one arena count and therefore
        // prevents object reclamation.
        unsafe { self.cell.as_ref() }
    }

    pub(crate) fn duplicate(&self) -> XllResult<Self> {
        // SAFETY: the boxed arena outlives every binding capability.
        unsafe { self.arena.as_ref() }.duplicate_binding(self.id, self.cell)?;
        Ok(Self {
            arena: self.arena,
            cell: self.cell,
            id: self.id,
            armed: true,
        })
    }

    #[cfg(any(feature = "async", test))]
    pub(crate) fn acquire_lease(&self) -> XllResult<RawObjectLeaseGuard> {
        // SAFETY: same lifetime invariant as `duplicate`.
        unsafe { self.arena.as_ref() }.acquire_pin(self.id, self.cell)
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
        if self.armed {
            // SAFETY: binding retirement waits for the read-domain grace
            // period before dropping this capability.
            unsafe { self.arena.as_ref() }.release_binding(self.id);
        }
    }
}

// SAFETY: ObjectBinding is a non-owning thread-safe handle to an arena-managed object.
unsafe impl Send for ObjectBinding {}
// SAFETY: ObjectBinding immutable borrows can be shared across threads.
unsafe impl Sync for ObjectBinding {}

/// Internal pin capability held by a generated async handle task.
///
/// This type deliberately has no public lifetime-bearing API. Its raw arena
/// pointer is safe only while the async task drain precedes handle-service
/// teardown; the shutdown pipeline owns that ordering invariant.
pub(crate) struct RawObjectLeaseGuard {
    arena: NonNull<ObjectArena>,
    id: ObjectId,
    armed: bool,
}

impl Drop for RawObjectLeaseGuard {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: an active pin prevents arena/service reclamation.
            unsafe { self.arena.as_ref() }.release_pin(self.id);
        }
    }
}

// SAFETY: RawObjectLeaseGuard holds a pin count in a thread-safe arena and can be transferred.
unsafe impl Send for RawObjectLeaseGuard {}
// SAFETY: RawObjectLeaseGuard immutable borrows can be shared across threads.
unsafe impl Sync for RawObjectLeaseGuard {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_pin_admission_preserves_both_counters() {
        let arena = ObjectArena::new();
        // SAFETY: all capabilities are dropped before this local arena.
        let binding = unsafe { arena.insert(ObjectId::new(1, 1), 42_u32) }.unwrap();
        for entry_overflow in [false, true] {
            {
                let mut state = arena.state.lock();
                state.active_pins = if entry_overflow { 0 } else { usize::MAX };
                state.objects.get_mut(&binding.id).unwrap().pins =
                    if entry_overflow { usize::MAX } else { 0 };
            }
            let result = arena.acquire_pin(binding.id, binding.cell);
            let mut state = arena.state.lock();
            let total = state.active_pins;
            let entry = state.objects.get_mut(&binding.id).unwrap();
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
            assert_eq!(pins, if entry_overflow { usize::MAX } else { 0 });
        }
        drop(binding);
    }
}
