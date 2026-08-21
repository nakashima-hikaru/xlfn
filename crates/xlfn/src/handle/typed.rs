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
    pub(crate) object_id: ObjectId,
    pub(crate) object_key: ObjectKey,
    pub(crate) value: NonNull<T>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(
        object_id: ObjectId,
        object_key: ObjectKey,
        value: NonNull<T>,
        _scope: &'call crate::CallScope<'call>,
    ) -> Self {
        Self {
            object_id,
            object_key,
            value,
            _call: PhantomData,
        }
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. A handle itself is never an Excel return value.
    pub fn alias(self) -> HandleAlias<'call, T> {
        HandleAlias {
            object_id: self.object_id,
            object_key: self.object_key,
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
        unsafe { self.value.as_ref() }
    }
}

/// A call-scoped capability that creates a formula binding to an existing
/// object identity.
///
/// This type carries only the object key. It is not an ownership extension;
/// the target binding must be installed before the surrounding call guard is
/// released.
pub struct HandleAlias<'call, T: ExcelHandleObject> {
    pub(crate) object_id: ObjectId,
    pub(crate) object_key: ObjectKey,
    pub(crate) _call: PhantomData<HandleAliasMarker<'call, T>>,
}

impl<T: ExcelHandleObject> HandleAlias<'_, T> {
    pub(crate) fn into_parts(self) -> (ObjectId, ObjectKey) {
        (self.object_id, self.object_key)
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
