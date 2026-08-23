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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// The sole shared owner of one erased handle payload.
pub(crate) type SharedObject = triomphe::Arc<ObjectCell>;

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
    lease_gate: Mutex<()>,
    #[cfg(any(test, feature = "unstable"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

impl ObjectLifetimeTracker {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            cleanup: Arc::new(HandleCleanupState::new()),
            live_objects: AtomicUsize::new(0),
            active_leases: AtomicUsize::new(0),
            sealed: AtomicBool::new(false),
            lease_gate: Mutex::new(()),
            #[cfg(any(test, feature = "unstable"))]
            ghost: std::sync::OnceLock::new(),
        })
    }

    pub(crate) fn cleanup(&self) -> &Arc<HandleCleanupState> {
        &self.cleanup
    }

    fn register_object(&self) -> XllResult<()> {
        self.live_objects
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            })
    }

    fn release_object(&self) {
        let previous = self.live_objects.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "handle object accounting is unbalanced");
    }

    fn acquire_lease(self: &Arc<Self>) -> XllResult<ObjectLeaseGuard> {
        let _gate = self.lease_gate.lock();
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
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandlePin);
        Ok(ObjectLeaseGuard {
            tracker: Arc::clone(self),
        })
    }

    fn release_lease(&self) {
        let previous = self.active_leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "handle lease accounting is unbalanced");
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandlePin);
    }

    pub(crate) fn seal(&self) {
        let _gate = self.lease_gate.lock();
        self.sealed.store(true, Ordering::Release);
    }

    pub(crate) fn finish_quiescence(&self) -> XllResult<()> {
        let _gate = self.lease_gate.lock();
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

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "unstable"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectDropReason {
    Normal = 0,
    BindingRemoved = 1,
    PublicationRollback = 2,
    Shutdown = 3,
}

impl ObjectDropReason {
    fn operation(self) -> &'static str {
        match self {
            Self::Normal => "handle object drop",
            Self::BindingRemoved => "handle binding removal",
            Self::PublicationRollback => "handle publication rollback",
            Self::Shutdown => "handle registry close",
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
    drop_reason: AtomicU8,
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
            drop_reason: AtomicU8::new(ObjectDropReason::Normal as u8),
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
    pub(crate) fn typed_ptr<T: 'static>(&self) -> Option<NonNull<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| self.ptr.cast())
    }

    pub(crate) fn mark_drop_reason(&self, reason: ObjectDropReason) {
        self.drop_reason.store(reason as u8, Ordering::Release);
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
        let reason = match self.drop_reason.load(Ordering::Acquire) {
            value if value == ObjectDropReason::BindingRemoved as u8 => {
                ObjectDropReason::BindingRemoved
            }
            value if value == ObjectDropReason::PublicationRollback as u8 => {
                ObjectDropReason::PublicationRollback
            }
            value if value == ObjectDropReason::Shutdown as u8 => ObjectDropReason::Shutdown,
            _ => ObjectDropReason::Normal,
        };
        let result = catch_unwind(AssertUnwindSafe(|| drop(owner)));
        if result.is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind(reason.operation(), &error);
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

// SAFETY: the guard contains only an Arc to atomics and a mutex-protected
// diagnostic state.
unsafe impl Send for ObjectLeaseGuard {}
// SAFETY: same invariant as `Send`.
unsafe impl Sync for ObjectLeaseGuard {}
