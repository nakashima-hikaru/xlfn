//! Call-scoped capabilities used while producing a return payload.

use crate::value::input::{InputMode, PlainInputMode};
#[cfg(feature = "handles")]
use crate::{XllResult, host_api::ExcelHost, input_identity::InputFingerprint};
use std::marker::PhantomData;
use std::rc::Rc;

type ServerIdentity<E, C> = fn(E, C) -> (E, C);
type ReturnContextMarker<'call, 'scope, E, C> =
    (Rc<()>, &'call (), &'scope (), ServerIdentity<E, C>);

/// Callback authority supplied by the admitted call frame.
#[doc(hidden)]
pub struct ReturnFrameAccess<'call> {
    pub(crate) udf_id: &'static str,
    pub(crate) callbacks: &'call crate::host_callback::HostCallbackSession,
}

/// Return capabilities selected by the UDF's input identity mode.
#[doc(hidden)]
pub struct ReturnContext<
    'call,
    'scope,
    M: InputMode = PlainInputMode,
    E: crate::RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    #[cfg_attr(
        not(feature = "handles"),
        allow(dead_code, reason = "Plain contexts carry only unit state")
    )]
    state: M::ReturnState<'call, 'scope, E, C>,
    lifetime: PhantomData<ReturnContextMarker<'call, 'scope, E, C>>,
}

impl<'call, 'scope, M: InputMode, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ReturnContext<'call, 'scope, M, E, C>
{
    pub(crate) fn from_state(state: M::ReturnState<'call, 'scope, E, C>) -> Self {
        Self {
            state,
            lifetime: PhantomData,
        }
    }
}

impl ReturnContext<'_, '_> {
    #[doc(hidden)]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: (),
            lifetime: PhantomData,
        }
    }
}
impl Default for ReturnContext<'_, '_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Required publication capability, retained only by formula return contexts.
#[cfg(feature = "handles")]
#[doc(hidden)]
pub struct FormulaPublisher<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
{
    runtime: crate::handle::FormulaHandleServiceResolver<'call>,
    udf_id: &'static str,
    inputs: InputFingerprint,
    host: ExcelHost<'scope>,
    server: PhantomData<ServerIdentity<E, C>>,
}

#[cfg(feature = "handles")]
impl<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    FormulaPublisher<'call, 'scope, E, C>
{
    pub(crate) fn new(
        handles: crate::value::input::HandleCallAccess<'call>,
        udf_id: &'static str,
        inputs: [u8; 32],
        callbacks: &'scope crate::host_callback::HostCallbackSession,
    ) -> Self {
        Self {
            runtime: handles.runtime,
            udf_id,
            inputs: InputFingerprint::from_bytes(inputs),
            host: ExcelHost::new(callbacks),
            server: PhantomData,
        }
    }
}

#[cfg(feature = "handles")]
impl<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ReturnContext<'call, 'scope, crate::value::FormulaInputMode, E, C>
{
    /// Selects the formula revision before invoking a handle producer.
    #[doc(hidden)]
    pub fn set_handle_reuse(&mut self, reuse_inputs: bool) -> XllResult<()> {
        if !reuse_inputs {
            self.state.inputs = crate::handle::fresh_invocation_inputs(self.state.inputs)?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn publish_existing_alias<'handle, T: crate::handle::ExcelHandleObject>(
        &mut self,
        operation: impl FnOnce() -> XllResult<crate::handle::HandleAlias<'handle, T>>,
    ) -> XllResult<String> {
        self.state.publish_existing_alias(operation)
    }
    #[doc(hidden)]
    pub fn publish_new_handle<T: crate::handle::ExcelHandleObject>(
        &mut self,
        operation: impl FnOnce() -> XllResult<T>,
    ) -> XllResult<String> {
        self.state.publish_new_handle(operation)
    }
}

#[cfg(feature = "handles")]
impl<'call, 'scope, E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>
    FormulaPublisher<'call, 'scope, E, C>
{
    fn publish_existing_alias<'handle, T>(
        &self,
        operation: impl FnOnce() -> XllResult<crate::handle::HandleAlias<'handle, T>>,
    ) -> XllResult<String>
    where
        T: crate::handle::ExcelHandleObject,
    {
        let access = self;
        let handles = access.runtime.get()?;
        let key = crate::handle::formula_revision_key(access.host, access.udf_id, access.inputs)?;
        let preparation =
            handles.prepare_observed_alias::<T, _>(key, operation()?, |key, token| {
                crate::excel_rtd::observe_handle::<E, C>(
                    handles,
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
        let key = crate::handle::formula_revision_key(access.host, access.udf_id, access.inputs)?;
        let preparation = handles.prepare_observed(key, operation, |key, token| {
            crate::excel_rtd::observe_handle::<E, C>(
                handles,
                crate::module_runtime::ingress(),
                key,
                token,
                access.host,
            )
        })?;
        Ok(preparation.into_token())
    }
}
