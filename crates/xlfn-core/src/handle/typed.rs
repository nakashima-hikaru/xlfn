use super::*;
use crate::{ExcelInputIdentity, InputIdentityEncoder};
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
///
/// A handle-producing UDF is memoized by its formula revision. For one live
/// formula revision, the producer is evaluated at most once and the resulting
/// handle token identifies that object for the token's entire lifetime.
pub trait ExcelHandleObject: Any + Send + Sync + 'static {}

/// A call-scoped read capability for an object owned by a formula handle.
///
/// The snapshot guard keeps the published object alive for the lifetime of
/// this value. The lifetime parameter is tied to the generated Excel call
/// scope, so a borrowed handle cannot escape the invocation that resolved it.
pub struct Handle<'call, T: ExcelHandleObject> {
    pub(crate) _snapshot: BindingSnapshot,
    pub(crate) binding_id: HandleId,
    pub(crate) object_id: ObjectId,
    pub(crate) value: NonNull<T>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(
        snapshot: BindingSnapshot,
        binding_id: HandleId,
        object_id: ObjectId,
        value: NonNull<T>,
        _scope: &'call crate::CallScope<'call>,
    ) -> Self {
        Self {
            _snapshot: snapshot,
            binding_id,
            object_id,
            value,
            _call: PhantomData,
        }
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. A handle itself is never an Excel return value.
    pub fn alias(self) -> HandleAlias<'call, T> {
        let record = self
            ._snapshot
            .get(self.binding_id.slot)
            .expect("a resolved handle must retain its binding record");
        debug_assert_eq!(record.id, self.binding_id);
        HandleAlias {
            object_id: self.object_id,
            object: Arc::clone(&record.object),
            _call: PhantomData,
            _type: PhantomData,
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `value` points into the `HandleObject` retained by the
        // published snapshot guard held in this value. The guard is dropped
        // only after this reference is no longer accessible.
        unsafe { self.value.as_ref() }
    }
}

impl<T: ExcelHandleObject> ExcelInputIdentity for Handle<'_, T> {
    fn input_identity(&self, encoder: &mut InputIdentityEncoder<'_>) {
        encoder.domain(b"xlfn.input.handle-object.v1");
        encoder.u64(self.object_id.0);
    }
}

/// A call-scoped capability that creates a formula binding to an existing
/// object identity.
pub struct HandleAlias<'call, T: ExcelHandleObject> {
    pub(crate) object_id: ObjectId,
    pub(crate) object: Arc<HandleObject>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
    pub(crate) _type: PhantomData<fn() -> T>,
}

impl<T: ExcelHandleObject> HandleAlias<'_, T> {
    pub(crate) fn into_parts(self) -> (ObjectId, Arc<HandleObject>) {
        (self.object_id, self.object)
    }
}

impl<'call, T: ExcelHandleObject> crate::ExcelReturn for HandleAlias<'call, T> {
    type Output = String;
    const USES_FORMULA_REVISION: bool = true;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        context.publish_existing_alias(|| Ok(self))
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<Self::Output> {
        context.publish_existing_alias(operation)
    }
}

impl<T: ExcelHandleObject> crate::value::MainThreadReturn for HandleAlias<'_, T> {}
