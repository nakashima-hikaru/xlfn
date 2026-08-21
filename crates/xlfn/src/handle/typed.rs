use super::*;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
///
/// A handle-producing UDF is memoized by its formula revision. For one live
/// formula revision, the producer is evaluated at most once and the resulting
/// handle token identifies that object for the token's entire lifetime.
pub trait ExcelHandleObject: Send + Sync + 'static {}

type HandleAliasMarker<'call, T> = (&'call crate::CallScope<'call>, fn() -> T);

/// A call-scoped read capability for an object owned by a formula handle.
///
/// The handle borrows an object-registry entry for the generated Excel call
/// scope. The call guard retains the runtime-local object arena and protects
/// the pointed-to value from epoch reclamation for exactly that scope.
pub struct Handle<'call, T: ExcelHandleObject> {
    pub(crate) object: ObjectLocator,
    pub(crate) value: TypedObjectRef<'call, T>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(object: ObjectLocator, value: TypedObjectRef<'call, T>) -> Self {
        Self {
            object,
            value,
            _call: PhantomData,
        }
    }

    /// Promotes this call-scoped capability to an owned registry lease.
    ///
    /// Promotion is explicit because it changes the lifetime kind: the
    /// resulting value may outlive the Excel call and keeps the registry
    /// payload alive until it is dropped.
    fn promote(self) -> XllResult<ObjectLease<T>> {
        let (pin, value) = self.value.guard().pin(self.object)?;
        Ok(ObjectLease::from_parts(pin, value))
    }

    /// Promotes this capability to a long-lived synchronous handle.
    pub fn pin(self) -> XllResult<PinnedHandle<T>> {
        self.promote().map(ObjectLease::into_pinned)
    }

    /// Promotes this capability to the handle type intended for an async
    /// future. The returned value is `Send + Sync + 'static`.
    pub fn into_async(self) -> XllResult<AsyncHandle<T>> {
        self.promote().map(ObjectLease::into_async)
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. A handle itself is never an Excel return value.
    pub fn alias(self) -> HandleAlias<'call, T> {
        HandleAlias {
            object: self.object,
            _call: PhantomData,
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: lookup validated the concrete type while the call epoch was
        // active. The epoch remains active for the lifetime of this handle, so
        // the object registry cannot reclaim the allocation underneath it.
        unsafe { self.value.as_ptr().as_ref() }
    }
}

/// Internal ownership layer shared by the two public long-lived handle
/// categories.
///
/// The lease owns a registry pin, not an `Arc<T>`: the registry remains the
/// sole payload owner and the pin only controls when that owner may be
/// retired.
struct ObjectLease<T: ExcelHandleObject> {
    value: NonNull<T>,
    pin: ObjectPin,
}

impl<T: ExcelHandleObject> ObjectLease<T> {
    fn from_parts(pin: ObjectPin, value: NonNull<T>) -> Self {
        Self { value, pin }
    }

    /// Returns the stable runtime-local object identity.
    pub fn object_id(&self) -> u64 {
        self.pin.object_id().0
    }

    /// Changes the synchronous lifetime marker without changing ownership.
    fn into_pinned(self) -> PinnedHandle<T> {
        PinnedHandle { lease: self }
    }

    /// Changes the async lifetime marker without changing ownership.
    fn into_async(self) -> AsyncHandle<T> {
        AsyncHandle { lease: self }
    }
}

impl<T: ExcelHandleObject> Deref for ObjectLease<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `pin` keeps the registry's sole payload owner alive for the
        // entire lifetime of this reference.
        unsafe { self.value.as_ref() }
    }
}

// SAFETY: the registry pin is Send + Sync and T is required to be Send + Sync;
// the pointer is only dereferenced while the pin is held.
unsafe impl<T: ExcelHandleObject> Send for ObjectLease<T> {}

// SAFETY: same invariant as Send.
unsafe impl<T: ExcelHandleObject> Sync for ObjectLease<T> {}

/// A long-lived synchronous handle. Use this when a handle must cross Excel
/// calls but remain in synchronous application code.
pub struct PinnedHandle<T: ExcelHandleObject> {
    lease: ObjectLease<T>,
}

impl<T: ExcelHandleObject> PinnedHandle<T> {
    /// Returns the stable runtime-local object identity.
    pub fn object_id(&self) -> u64 {
        self.lease.object_id()
    }

    pub fn into_async(self) -> AsyncHandle<T> {
        self.lease.into_async()
    }
}

impl<T: ExcelHandleObject> Deref for PinnedHandle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lease
    }
}

/// A long-lived handle whose ownership is explicitly suitable for an async
/// future. It owns the same registry pin as [`PinnedHandle`] but is a separate
/// type so async APIs cannot accidentally capture [`Handle<'_, T>`].
pub struct AsyncHandle<T: ExcelHandleObject> {
    lease: ObjectLease<T>,
}

impl<T: ExcelHandleObject> AsyncHandle<T> {
    /// Returns the stable runtime-local object identity.
    pub fn object_id(&self) -> u64 {
        self.lease.object_id()
    }

    pub fn into_pinned(self) -> PinnedHandle<T> {
        PinnedHandle { lease: self.lease }
    }
}

impl<T: ExcelHandleObject> Deref for AsyncHandle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.lease
    }
}

/// A call-scoped capability that creates a formula binding to an existing
/// object identity.
///
/// This type carries only the object identity and current storage key. It is
/// not an ownership extension; the target binding must be installed before the
/// surrounding call guard is released. If the source binding was retired in
/// the same call, publication can recover its payload from the epoch-retired
/// queue and assign a fresh storage key.
pub struct HandleAlias<'call, T: ExcelHandleObject> {
    pub(crate) object: ObjectLocator,
    pub(crate) _call: PhantomData<HandleAliasMarker<'call, T>>,
}

impl<T: ExcelHandleObject> HandleAlias<'_, T> {
    pub(crate) fn into_locator(self) -> ObjectLocator {
        self.object
    }
}

impl<'call, T: ExcelHandleObject> crate::ExcelReturn for HandleAlias<'call, T> {
    const USES_FORMULA_REVISION: bool = true;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<crate::ExcelOutput> {
        context
            .publish_existing_alias(|| Ok(self))
            .map(|token| crate::ExcelOutput::Scalar(crate::ExcelCellOutput::String(token)))
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<crate::ExcelOutput> {
        context
            .publish_existing_alias(operation)
            .map(|token| crate::ExcelOutput::Scalar(crate::ExcelCellOutput::String(token)))
    }
}

impl<T: ExcelHandleObject> crate::value::MainThreadReturn for HandleAlias<'_, T> {}
