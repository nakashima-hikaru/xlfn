use super::binding::BindingReadLease;
use super::object::{ObjectBinding, ObjectLeaseGuard, TypedObjectProjection};
use super::token::ObjectId;
use crate::XllResult;
use std::marker::PhantomData;
use std::ops::Deref;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
pub trait ExcelHandleObject: Send + Sync + 'static {}

type HandleAliasMarker<'call, T> = (&'call crate::call::CallScope<'call>, fn() -> T);

/// Opaque identity of a registry-owned handle payload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HandleObjectId {
    session: u64,
    sequence: u64,
}

impl HandleObjectId {
    pub(crate) const fn from_object_id(id: ObjectId) -> Self {
        Self {
            session: id.session(),
            sequence: id.sequence(),
        }
    }

    pub(crate) const fn session(self) -> u64 {
        self.session
    }

    pub(crate) const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// A call-scoped read capability for an object owned by a formula binding.
///
/// The binding read lease anchors the immutable publication snapshot. That
/// snapshot points to a slot-owned binding record, which holds an
/// `ObjectBinding` capability into the `ObjectArena`-owned `ObjectCell`.
/// A warm lookup therefore does not clone the object `Arc`.
pub struct Handle<'call, T: ExcelHandleObject> {
    pub(crate) binding: BindingReadLease,
    pub(crate) value: TypedObjectProjection<T>,
    pub(crate) _call: PhantomData<&'call crate::call::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(binding: BindingReadLease, value: TypedObjectProjection<T>) -> Self {
        Self {
            binding,
            value,
            _call: PhantomData,
        }
    }

    /// Returns the session-scoped identity used by formula input semantics.
    pub(crate) fn object_id(&self) -> ObjectId {
        self.binding.object().id()
    }

    /// Promotes this call-scoped capability to an owned handle lease.
    ///
    /// The lease contributes a pin capability to the shutdown certificate;
    /// it never shares ownership of the payload allocation.
    pub fn pin(self) -> XllResult<HandleLease<T>> {
        let object_id = self.binding.object().id();
        let lease = self.binding.acquire_object_lease()?;
        let value = self
            .binding
            .object()
            .typed_projection::<T>()
            .expect("handle type was validated before promotion");
        Ok(HandleLease {
            object_id,
            value,
            _lease: lease,
        })
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. The source snapshot remains the alias's lifetime anchor
    /// until publication consumes it.
    pub fn alias(self) -> HandleAlias<'call, T> {
        HandleAlias {
            binding: self.binding,
            _call: PhantomData,
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        let _anchor = &self.binding;
        // SAFETY: `value` points into the `ObjectCell` transitively owned by
        // `binding`, and that cell cannot be dropped while this binding lease
        // is alive.
        self.value.as_ref()
    }
}

/// A long-lived handle lease. Use this when a handle must cross Excel calls or
/// be moved into an asynchronous future.
pub struct HandleLease<T: ExcelHandleObject> {
    pub(crate) object_id: ObjectId,
    pub(crate) value: TypedObjectProjection<T>,
    pub(crate) _lease: ObjectLeaseGuard,
}

impl<T: ExcelHandleObject> HandleLease<T> {
    /// Returns the stable session-scoped object identity.
    pub fn object_id(&self) -> HandleObjectId {
        HandleObjectId::from_object_id(self.object_id)
    }
}

impl<T: ExcelHandleObject> Deref for HandleLease<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: `object` owns the payload for the entire lifetime of this
        // lease, and `value` was created from that same object cell.
        self.value.as_ref()
    }
}

// SAFETY: the object cell and lease guard are Send/Sync, and T is constrained
// by ExcelHandleObject.
unsafe impl<T: ExcelHandleObject> Send for HandleLease<T> {}
// SAFETY: same invariant as `Send`.
unsafe impl<T: ExcelHandleObject> Sync for HandleLease<T> {}

/// A call-scoped capability that creates a formula binding to an existing
/// object. It carries the source binding snapshot directly, so address-reuse
/// and resurrection machinery are unnecessary.
pub struct HandleAlias<'call, T: ExcelHandleObject> {
    pub(crate) binding: BindingReadLease,
    pub(crate) _call: PhantomData<HandleAliasMarker<'call, T>>,
}

impl<T: ExcelHandleObject> HandleAlias<'_, T> {
    pub(crate) fn into_object_binding(self) -> XllResult<ObjectBinding> {
        self.binding.duplicate_object_binding()
    }

    #[cfg(test)]
    pub(crate) fn object_id(&self) -> ObjectId {
        self.binding.object().id()
    }
}
