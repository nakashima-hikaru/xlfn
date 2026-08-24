//! Shared ownership for formula-handle payloads.
//!
//! A published binding owns one strong reference to an [`ObjectCell`].  A
//! call-scoped binding snapshot keeps that reference alive without another
//! atomic increment on the lookup path.  Only an explicit [`Handle::pin`]
//! creates an additional long-lived lease.

use super::token::ObjectId;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::any::{Any, TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The sole shared owner of one erased handle payload.
pub(crate) type SharedObject = triomphe::Arc<ObjectCell>;

/// A type-checked, non-owning projection into an [`ObjectCell`].
///
/// The projection is created only by `ObjectCell::typed_projection`. Callers
/// must retain the corresponding `SharedObject` or binding snapshot while
/// using it; `Handle` and `HandleLease` encode that ownership around this
/// value. Keeping the pointer construction and dereference proof here avoids
/// duplicating raw-pointer casts in each public handle type.
pub(crate) struct TypedObjectProjection<T: 'static> {
    pointer: NonNull<T>,
}

impl<T: 'static> TypedObjectProjection<T> {
    #[cfg(test)]
    pub(crate) fn addr(&self) -> usize {
        self.pointer.addr().get()
    }

    #[inline]
    pub(crate) fn as_ref(&self) -> &T {
        // SAFETY: this projection is constructed only after ObjectCell checks
        // its TypeId, and its enclosing handle retains the ObjectCell owner.
        unsafe { self.pointer.as_ref() }
    }
}

/// Errors raised while a handle payload is being destroyed are retained until
/// the handle subsystem presents its quiescence certificate.
pub(crate) struct HandleCleanupState {
    failure: Mutex<Option<XllError>>,
}

impl HandleCleanupState {
    pub(crate) fn new() -> Self {
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

/// Lifetime accounting owned by one handle registry.
pub(crate) struct ObjectLifetimeTracker {
    cleanup: Arc<HandleCleanupState>,
    live_objects: AtomicUsize,
    active_leases: AtomicUsize,
    sealed: AtomicBool,
    admission_gate: Mutex<()>,
    #[cfg(any(test, feature = "refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

impl ObjectLifetimeTracker {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            cleanup: Arc::new(HandleCleanupState::new()),
            live_objects: AtomicUsize::new(0),
            active_leases: AtomicUsize::new(0),
            sealed: AtomicBool::new(false),
            admission_gate: Mutex::new(()),
            #[cfg(any(test, feature = "refinement"))]
            ghost: std::sync::OnceLock::new(),
        })
    }

    pub(crate) fn cleanup(&self) -> &Arc<HandleCleanupState> {
        &self.cleanup
    }

    fn register_object(&self) -> XllResult<()> {
        let _gate = self.admission_gate.lock();
        if self.sealed.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        self.live_objects
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            })
            .inspect(|()| {
                #[cfg(any(test, feature = "refinement"))]
                self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandleObject);
            })
    }

    fn release_object(&self) {
        let previous = self.live_objects.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "handle object accounting is unbalanced");
        #[cfg(any(test, feature = "refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandleObject);
    }

    fn acquire_lease(self: &Arc<Self>) -> XllResult<ObjectLeaseGuard> {
        let _gate = self.admission_gate.lock();
        if self.sealed.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        self.active_leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            })?;
        #[cfg(any(test, feature = "refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandlePin);
        Ok(ObjectLeaseGuard {
            tracker: Arc::clone(self),
        })
    }

    fn release_lease(&self) {
        let previous = self.active_leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "handle lease accounting is unbalanced");
        #[cfg(any(test, feature = "refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandlePin);
    }

    pub(crate) fn seal(&self) {
        let _gate = self.admission_gate.lock();
        self.sealed.store(true, Ordering::Release);
    }

    pub(crate) fn finish_quiescence(&self) -> XllResult<()> {
        let _gate = self.admission_gate.lock();
        if self.active_leases.load(Ordering::Acquire) != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_PINS,
            });
        }
        if self.live_objects.load(Ordering::Acquire) != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_OBJECTS,
            });
        }
        self.cleanup.result()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }
}

/// Stable storage for one typed handle payload.
pub(crate) struct ObjectCell {
    id: ObjectId,
    owner: Option<Box<dyn Any + Send + Sync>>,
    ptr: NonNull<()>,
    type_id: TypeId,
    type_name: &'static str,
    lifetime: Arc<ObjectLifetimeTracker>,
}

impl ObjectCell {
    pub(crate) fn new<T: Send + Sync + 'static>(
        id: ObjectId,
        value: T,
        lifetime: Arc<ObjectLifetimeTracker>,
    ) -> XllResult<SharedObject> {
        lifetime.register_object()?;
        let owner: Box<dyn Any + Send + Sync> = Box::new(value);
        let ptr = NonNull::from_ref(owner.as_ref()).cast::<()>();
        Ok(triomphe::Arc::new(Self {
            id,
            owner: Some(owner),
            ptr,
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            lifetime,
        }))
    }

    pub(crate) fn id(&self) -> ObjectId {
        self.id
    }

    pub(crate) fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub(crate) fn type_name(&self) -> &'static str {
        self.type_name
    }

    #[inline]
    pub(crate) fn typed_projection<T: 'static>(&self) -> Option<TypedObjectProjection<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| TypedObjectProjection {
            pointer: self.ptr.cast(),
        })
    }

    pub(crate) fn acquire_lease(&self) -> XllResult<ObjectLeaseGuard> {
        self.lifetime.acquire_lease()
    }
}

// SAFETY: `owner` is the sole allocation owner and is explicitly Send + Sync.
// `ptr` is only a stable, non-owning pointer into that allocation and is
// dereferenced only while an owning `SharedObject` is held.
unsafe impl Send for ObjectCell {}
// SAFETY: same invariant as `Send`.
unsafe impl Sync for ObjectCell {}

impl Drop for ObjectCell {
    fn drop(&mut self) {
        let owner = self
            .owner
            .take()
            .expect("object cell owner must be present exactly once");
        let result = catch_unwind(AssertUnwindSafe(|| drop(owner)));
        if result.is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle object final drop", &error);
            self.lifetime.cleanup.record(error);
        }
        self.lifetime.release_object();
    }
}

/// The explicit long-lived ownership witness created by `Handle::pin()`.
pub(crate) struct ObjectLeaseGuard {
    tracker: Arc<ObjectLifetimeTracker>,
}

impl Drop for ObjectLeaseGuard {
    fn drop(&mut self) {
        self.tracker.release_lease();
    }
}
