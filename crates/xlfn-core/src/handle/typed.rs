use super::*;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
///
/// A handle-producing UDF is memoized by its formula identity. For one live
/// formula identity, the producer is evaluated at most once and the resulting
/// handle token identifies that object for the token's entire lifetime.
pub trait ExcelHandleObject: Any + Send + Sync + 'static {}

/// A call-scoped read capability for an object owned by a formula handle.
///
/// The snapshot guard keeps the published object alive for the lifetime of
/// this value. The lifetime parameter is tied to the generated Excel call
/// scope, so a borrowed handle cannot escape the invocation that resolved it.
pub struct Handle<'call, T: ExcelHandleObject> {
    pub(crate) snapshot: PublishedHandleSnapshot,
    pub(crate) slot: u32,
    pub(crate) value: NonNull<T>,
    pub(crate) _call: PhantomData<&'call crate::CallScope<'call>>,
}

impl<'call, T: ExcelHandleObject> Handle<'call, T> {
    pub(crate) fn new(
        snapshot: PublishedHandleSnapshot,
        slot: u32,
        value: NonNull<T>,
        _scope: &'call crate::CallScope<'call>,
    ) -> Self {
        Self {
            snapshot,
            slot,
            value,
            _call: PhantomData,
        }
    }

    /// Converts this borrowed capability into an explicit republish
    /// capability. A handle itself is never an Excel return value.
    pub fn alias(self) -> HandleAlias<'call, T> {
        HandleAlias { handle: self }
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

/// A call-scoped capability that republishes an existing formula-owned
/// object under the current formula identity.
pub struct HandleAlias<'call, T: ExcelHandleObject> {
    handle: Handle<'call, T>,
}

impl<T: ExcelHandleObject> HandleAlias<'_, T> {
    pub(crate) fn into_object(self) -> Arc<HandleObject> {
        let handle = self.handle;
        let publication = handle
            .snapshot
            .get(&handle.slot)
            .expect("a live handle alias retains its publication");
        Arc::clone(&publication.object)
    }
}

impl<'call, T: ExcelHandleObject> crate::ExcelReturn for HandleAlias<'call, T> {
    type Output = String;

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
