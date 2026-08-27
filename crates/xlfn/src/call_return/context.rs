//! Call-scoped capabilities used while producing a return payload.

#[cfg(feature = "handles")]
use crate::XllResult;
#[cfg(feature = "handles")]
use crate::host_api::ExcelHost;
#[cfg(feature = "handles")]
use crate::input_identity::InputFingerprint;
use std::marker::PhantomData;
use std::rc::Rc;

/// Call-scoped services used by [`super::ExcelReturn`] implementations.
#[doc(hidden)]
pub struct ReturnContext<'call, 'scope> {
    #[cfg(feature = "handles")]
    publisher: Option<FormulaPublisher<'call, 'scope>>,
    lifetime: PhantomData<(Rc<()>, &'call (), &'scope ())>,
}

/// Capability for publishing a handle result for one formula revision.
#[cfg(feature = "handles")]
pub(crate) struct FormulaPublisher<'call, 'scope> {
    pub(crate) runtime: crate::handle::FormulaHandleServiceResolver<'call>,
    pub(crate) udf_id: &'static str,
    pub(crate) inputs: InputFingerprint,
    pub(crate) host: ExcelHost<'scope>,
}

impl<'call, 'scope> ReturnContext<'call, 'scope> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            #[cfg(feature = "handles")]
            publisher: None,
            lifetime: PhantomData,
        }
    }

    #[doc(hidden)]
    /// Creates return services for one generated synchronous UDF call.
    pub fn for_call<A: crate::Addin>(
        call: &'call crate::runtime::CallGuard<'_, A>,
        udf_id: &'static str,
        inputs: Option<[u8; 32]>,
        scope: &'scope crate::call::CallScope<'scope>,
    ) -> Self {
        #[cfg(feature = "handles")]
        let publisher = inputs.map(|inputs| FormulaPublisher {
            runtime: call.handle_call_access(),
            udf_id,
            inputs: InputFingerprint::from_bytes(inputs),
            host: ExcelHost::new(scope.callbacks()),
        });
        #[cfg(not(feature = "handles"))]
        let _ = (call, udf_id, inputs, scope);
        Self {
            #[cfg(feature = "handles")]
            publisher,
            lifetime: PhantomData,
        }
    }

    #[cfg(feature = "handles")]
    fn publisher(&self) -> XllResult<&FormulaPublisher<'call, 'scope>> {
        self.publisher.as_ref().ok_or(crate::XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_CONTEXT,
        })
    }
}

#[cfg(feature = "handles")]
impl<'call> ReturnContext<'call, 'call> {
    pub(crate) fn for_frame(
        handles: crate::value::input::HandleCallAccess<'call>,
        udf_id: &'static str,
        inputs: Option<[u8; 32]>,
    ) -> Self {
        let publisher = inputs.map(|inputs| FormulaPublisher {
            runtime: handles.runtime,
            udf_id,
            inputs: InputFingerprint::from_bytes(inputs),
            host: ExcelHost::new(handles.scope.callbacks()),
        });
        Self {
            publisher,
            lifetime: PhantomData,
        }
    }
}

#[cfg(feature = "handles")]
impl<'call, 'scope> ReturnContext<'call, 'scope> {
    #[doc(hidden)]
    pub fn publish_existing_alias<'handle, T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<crate::handle::HandleAlias<'handle, T>>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        self.publisher()?.publish_existing_alias(operation)
    }

    #[doc(hidden)]
    pub fn publish_new_handle<T>(
        &mut self,
        operation: impl FnOnce() -> XllResult<T>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        self.publisher()?.publish_new_handle(operation)
    }
}

#[cfg(feature = "handles")]
impl<'call, 'scope> FormulaPublisher<'call, 'scope> {
    fn publish_existing_alias<'handle, T>(
        &self,
        operation: impl FnOnce() -> XllResult<crate::handle::HandleAlias<'handle, T>>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        let access = self;
        let handles = access.runtime.get()?;
        let arc_handles = std::sync::Arc::clone(access.runtime.get_arc()?);
        let key = crate::handle::formula_revision_key(access.host, access.udf_id, access.inputs)?;
        let preparation =
            handles.prepare_observed_alias::<T, _>(key, operation()?, |key, token| {
                crate::excel_rtd::observe_handle(
                    arc_handles,
                    crate::module_runtime::ingress(),
                    key,
                    token,
                    access.host,
                )
            })?;
        Ok(preparation.into_token())
    }

    fn publish_new_handle<T>(&self, operation: impl FnOnce() -> XllResult<T>) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        let access = self;
        let handles = access.runtime.get()?;
        let arc_handles = std::sync::Arc::clone(access.runtime.get_arc()?);
        let key = crate::handle::formula_revision_key(access.host, access.udf_id, access.inputs)?;
        let preparation = handles.prepare_observed(key, operation, |key, token| {
            crate::excel_rtd::observe_handle(
                arc_handles,
                crate::module_runtime::ingress(),
                key,
                token,
                access.host,
            )
        })?;
        Ok(preparation.into_token())
    }
}

impl Default for ReturnContext<'_, '_> {
    fn default() -> Self {
        Self::new()
    }
}
