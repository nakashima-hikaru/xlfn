use super::*;
use std::marker::PhantomData;
use std::ops::Deref;

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
    pub(crate) object: LiveObjectRef,
    pub(crate) value: BorrowedObject<'call, T>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(object: LiveObjectRef, value: BorrowedObject<'call, T>) -> Self {
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
    fn promote(self) -> XllResult<PinnedObject<T>> {
        self.value.guard().pin(self.object)
    }

    /// Promotes this capability to a long-lived synchronous handle.
    pub fn pin(self) -> XllResult<PinnedHandle<T>> {
        self.promote().map(|lease| PinnedHandle { lease })
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. A handle itself is never an Excel return value.
    pub fn alias(self) -> HandleAlias<'call, T> {
        HandleAlias {
            object: ObjectLocator {
                id: self.object.id,
                key_hint: self.object.key,
            },
            _call: PhantomData,
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// A long-lived handle. Use this when a handle must cross Excel calls or be
/// moved into an asynchronous future.
pub struct PinnedHandle<T: ExcelHandleObject> {
    lease: PinnedObject<T>,
}

impl<T: ExcelHandleObject> PinnedHandle<T> {
    /// Returns the stable runtime-local object identity.
    pub fn object_id(&self) -> u64 {
        self.lease.object_id()
    }
}

impl<T: ExcelHandleObject> Deref for PinnedHandle<T> {
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
