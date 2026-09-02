//! Runtime-owned arena for formula-handle payloads.
//!
//! The arena is the unique owner of every [`ObjectCell`]. Bindings and pins
//! carry counted, non-owning capabilities; neither participates in memory
//! ownership. An object is reclaimed only after both capability counts reach
//! zero, and its application destructor always runs outside the arena lock.

use super::token::ObjectId;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

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

    #[inline]
    pub(crate) fn as_ref(&self) -> &T {
        // SAFETY: projections are constructed only after a TypeId check. The
        // enclosing binding-read or pin capability delays object reclamation.
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
    cell: Box<ObjectCell>,
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

    pub(crate) fn insert<T: Send + Sync + 'static>(
        &self,
        id: ObjectId,
        value: T,
    ) -> XllResult<ObjectBinding> {
        let owner: Box<dyn Any + Send + Sync> = Box::new(value);
        let pointer = NonNull::from_ref(owner.as_ref()).cast::<()>();
        let cell = Box::new(ObjectCell {
            id,
            owner: Some(owner),
            pointer,
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
        });
        let cell_pointer = NonNull::from(cell.as_ref());
        let mut state = self.state.lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        if state
            .objects
            .insert(
                id,
                ObjectEntry {
                    cell,
                    bindings: 1,
                    pins: 0,
                },
            )
            .is_some()
        {
            xlfn_kernel::invariant::fail_stop();
        }
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

    fn acquire_pin(&self, id: ObjectId, cell: NonNull<ObjectCell>) -> XllResult<ObjectLeaseGuard> {
        let mut state = self.state.lock();
        if state.sealed {
            return Err(XllError::Closing);
        }
        let entry = state.objects.get_mut(&id).ok_or(XllError::StaleHandle)?;
        if NonNull::from(entry.cell.as_ref()) != cell {
            return Err(XllError::StaleHandle);
        }
        entry.pins = entry.pins.checked_add(1).ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;
        state.active_pins = state.active_pins.checked_add(1).ok_or(XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;
        drop(state);
        self.record(crate::shutdown_trace::ShutdownEvent::AddHandlePin);
        Ok(ObjectLeaseGuard {
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

    fn destroy(&self, mut cell: Box<ObjectCell>) {
        let owner = cell
            .owner
            .take()
            .expect("object cell owner is consumed exactly once");
        if catch_unwind(AssertUnwindSafe(|| drop(owner))).is_err() {
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

    pub(crate) fn acquire_lease(&self) -> XllResult<ObjectLeaseGuard> {
        // SAFETY: same lifetime invariant as `duplicate`.
        unsafe { self.arena.as_ref() }.acquire_pin(self.id, self.cell)
    }
}

impl Drop for ObjectBinding {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: registry sealing waits for binding retirement before
            // reclaiming the boxed arena.
            unsafe { self.arena.as_ref() }.release_binding(self.id);
        }
    }
}

// SAFETY: ObjectBinding is a non-owning thread-safe handle to an arena-managed object.
unsafe impl Send for ObjectBinding {}
// SAFETY: ObjectBinding immutable borrows can be shared across threads.
unsafe impl Sync for ObjectBinding {}

pub(crate) struct ObjectLeaseGuard {
    arena: NonNull<ObjectArena>,
    id: ObjectId,
    armed: bool,
}

impl Drop for ObjectLeaseGuard {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: an active pin prevents arena/service reclamation.
            unsafe { self.arena.as_ref() }.release_pin(self.id);
        }
    }
}

// SAFETY: ObjectLeaseGuard holds a pin count in a thread-safe arena and can be transferred.
unsafe impl Send for ObjectLeaseGuard {}
// SAFETY: ObjectLeaseGuard immutable borrows can be shared across threads.
unsafe impl Sync for ObjectLeaseGuard {}
