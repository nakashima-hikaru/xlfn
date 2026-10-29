use super::binding::BindingReadLease;
use super::object::{PendingObjectBinding, RawObjectLeaseGuard, TypedObjectProjection};
use super::store::HandleStore;
use super::token::ObjectId;
#[cfg(any(feature = "async", test))]
use crate::generation::RuntimeGeneration;
use crate::panic_boundary::catch_no_unwind;
use crate::{XllError, XllResult};
use std::any::type_name;
use std::marker::PhantomData;
use std::ops::Deref;
use std::panic::AssertUnwindSafe;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
pub trait ExcelHandleObject: Send + Sync + 'static {}

/// Compile-time brand used to keep a generated async handle lease inside the
/// task that acquired its raw object pin.
pub(crate) struct GenerationLeaseBrand;

type HandleAliasMarker<'call, T> = (&'call crate::call::CallScope<'call>, fn(T) -> T);

/// Opaque identity of a registry-owned handle payload.
///
/// All bindings and aliases for the same live object share this identity. It
/// is scoped to the current handle session, so it must not be persisted across
/// workbook saves or Excel restarts. Holding an ID does not keep the object
/// alive, and an ID cannot be used to reconstruct a handle.
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
/// A warm lookup therefore only projects the registry-owned object.
pub struct Handle<'call, T: ExcelHandleObject> {
    binding: BindingReadLease<'call>,
    value: TypedObjectProjection<T>,
    _call: PhantomData<&'call crate::call::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    #[inline]
    pub(super) fn new(binding: BindingReadLease<'call>) -> XllResult<Self> {
        let Some(value) = binding.object().typed_projection::<T>() else {
            let actual_type = binding.object().type_name();
            let _ = catch_no_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        };
        Ok(Self {
            binding,
            value,
            _call: PhantomData,
        })
    }

    #[cfg(test)]
    pub(super) fn projection_addr(&self) -> usize {
        self.value.addr()
    }

    /// Returns the session-scoped identity of the underlying object.
    ///
    /// Another handle obtained from an alias of this object has the same ID.
    /// Retaining the ID does not extend the object's lifetime. See
    /// [`HandleObjectId`] for the persistence limits.
    #[must_use]
    pub fn object_id(&self) -> HandleObjectId {
        HandleObjectId::from_object_id(self.binding.object().id())
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
        unsafe { self.value.as_ref_unchecked() }
    }
}

impl<T: ExcelHandleObject> AsRef<T> for Handle<'_, T> {
    #[inline]
    fn as_ref(&self) -> &T {
        self
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
    /// Returns the session-scoped identity of the underlying object.
    ///
    /// Synchronous handles and aliases of this object share this ID. Retaining
    /// the ID does not extend the object's lifetime. See [`HandleObjectId`] for
    /// the persistence limits.
    #[must_use]
    pub fn object_id(&self) -> HandleObjectId {
        HandleObjectId::from_object_id(self.object_id)
    }
}

impl<T: ExcelHandleObject> Deref for HandleLease<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: `_lease` owns the raw object lease guard for the entire lifetime of this
        // lease, and `value` was created from that same object cell.
        unsafe { self.value.as_ref_unchecked() }
    }
}

impl<T: ExcelHandleObject> AsRef<T> for HandleLease<'_, T> {
    #[inline]
    fn as_ref(&self) -> &T {
        self
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
    unreachable_pub,
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
    binding: BindingReadLease<'call>,
    _call: PhantomData<HandleAliasMarker<'call, T>>,
}

impl<'call, T: ExcelHandleObject> HandleAlias<'call, T> {
    pub(super) fn binding(&self) -> &BindingReadLease<'call> {
        &self.binding
    }

    pub(crate) fn into_pending_object_binding<'store>(
        self,
        store: &'store HandleStore,
    ) -> XllResult<PendingObjectBinding<'store, T>> {
        store.duplicate_binding(self)
    }

    #[cfg(test)]
    pub(crate) fn object_id(&self) -> ObjectId {
        self.binding.object().id()
    }
}
