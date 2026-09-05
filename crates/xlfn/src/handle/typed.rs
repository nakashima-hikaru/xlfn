use super::binding::BindingReadLease;
use super::object::{ObjectBinding, RawObjectLeaseGuard, TypedObjectProjection};
use super::token::ObjectId;
use crate::XllResult;
#[cfg(any(feature = "async", test))]
use crate::generation::RuntimeGeneration;
use std::marker::PhantomData;
use std::ops::Deref;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
pub trait ExcelHandleObject: Send + Sync + 'static {}

/// Compile-time brand used to keep a generated async handle lease inside the
/// task that acquired its raw object pin.
pub(crate) struct GenerationLeaseBrand;

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

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Handle identity accessors are used by protocol tests"
    )]
    pub(crate) const fn session(self) -> u64 {
        self.session
    }

    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "Handle identity accessors are used by protocol tests"
    )]
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

    /// Converts this call-scoped capability into the internal pending form
    /// used by the generated async-UDF launch path.
    #[cfg(any(feature = "async", test))]
    pub(crate) fn into_pending(
        self,
        generation: RuntimeGeneration,
    ) -> XllResult<PendingHandleLease<T>> {
        let object_id = self.binding.object().id();
        let lease = self.binding.acquire_object_lease()?;
        Ok(PendingHandleLease {
            object_id,
            value: self.value,
            lease,
            generation,
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

/// A generation-scoped handle lease supplied to an async UDF.
///
/// The framework creates this value only after the generated task has been
/// admitted. It is intentionally tied to the task's branded generation and
/// cannot be returned from the UDF, stored in a `'static` location, or moved
/// into an independently spawned thread.
pub struct HandleLease<'generation, T: ExcelHandleObject> {
    pub(crate) object_id: ObjectId,
    pub(crate) value: TypedObjectProjection<T>,
    pub(crate) _lease: RawObjectLeaseGuard,
    pub(crate) _generation: PhantomData<&'generation GenerationLeaseBrand>,
}

impl<T: ExcelHandleObject> HandleLease<'_, T> {
    /// Returns the stable session-scoped object identity.
    pub fn object_id(&self) -> HandleObjectId {
        HandleObjectId::from_object_id(self.object_id)
    }
}

impl<T: ExcelHandleObject> Deref for HandleLease<'_, T> {
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
unsafe impl<T: ExcelHandleObject> Send for HandleLease<'_, T> {}
// SAFETY: same invariant as `Send`.
unsafe impl<T: ExcelHandleObject> Sync for HandleLease<'_, T> {}

/// Decode-time handle lease that owns the object pin before the Excel call
/// returns. It is converted to [`HandleLease`] at the single async task
/// boundary, once the generation brand is available.
#[cfg(any(feature = "async", test))]
#[doc(hidden)]
#[allow(
    dead_code,
    reason = "The pending fields are consumed by the async scoped path"
)]
pub struct PendingHandleLease<T: ExcelHandleObject> {
    pub(crate) object_id: ObjectId,
    pub(crate) value: TypedObjectProjection<T>,
    pub(crate) lease: RawObjectLeaseGuard,
    pub(crate) generation: RuntimeGeneration,
}

#[cfg(all(feature = "async", feature = "handles"))]
impl<T: ExcelHandleObject> PendingHandleLease<T> {
    pub(crate) fn bind<'generation>(
        self,
        scope: crate::async_udf::AsyncTaskScope<'generation>,
    ) -> HandleLease<'generation, T> {
        debug_assert_eq!(self.generation, scope.generation());
        HandleLease {
            object_id: self.object_id,
            value: self.value,
            _lease: self.lease,
            _generation: PhantomData,
        }
    }
}

// SAFETY: the projection is guarded by the arena pin, and the payload is
// constrained by `ExcelHandleObject` to be Send/Sync.
#[cfg(any(feature = "async", test))]
unsafe impl<T: ExcelHandleObject> Send for PendingHandleLease<T> {}
// SAFETY: same invariant as `Send`.
#[cfg(any(feature = "async", test))]
unsafe impl<T: ExcelHandleObject> Sync for PendingHandleLease<T> {}

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
