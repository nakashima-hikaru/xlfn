//! Worksheet-input conversion and call-boundary state.

use crate::host_callback::HostCallbackSession;
use crate::input_identity::{InputFingerprintBuilder, InputIdentityEncoder};
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use xlfn_sys::{XLOPER12, XLTYPE_MISSING, XLTYPE_NIL};

use super::{ExcelReturn, XlValueRef, encode_raw_value};

/// Converts a call-scoped Excel value into owned Rust data.
pub trait FromExcel<'call>: Sized {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;
}

/// Framework-side argument dispatch used by generated ABI wrappers.
#[doc(hidden)]
pub trait ExcelParameter<'call>: Sized {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self>;
}

impl<'call, T: FromExcel<'call>> ExcelParameter<'call> for T {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let result = T::from_excel(value, argument)?;
        if let Some(identity) = identity {
            encode_raw_value(value, false, identity);
        }
        Ok(result)
    }
}

/// Runtime services available while converting one Excel-visible argument.
#[doc(hidden)]
pub struct CallContext<'call> {
    handle_runtime: Option<crate::handle::HandleRuntimeResolver<'call>>,
    scope: Option<&'call CallScope<'call>>,
}

impl<'call> CallContext<'call> {
    pub(crate) fn new<A: crate::Addin>(
        runtime: &'call crate::Runtime<A>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        Self {
            handle_runtime: Some(crate::handle::HandleRuntimeResolver::new(
                runtime.handle_runtime_slot(),
            )),
            scope: Some(scope),
        }
    }

    pub(crate) const fn without_runtime() -> Self {
        Self {
            handle_runtime: None,
            scope: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_resolver(
        handle_runtime: Option<crate::handle::HandleRuntimeResolver<'call>>,
        scope: Option<&'call CallScope<'call>>,
    ) -> Self {
        Self {
            handle_runtime,
            scope,
        }
    }

    pub(crate) fn take_handle_runtime(
        &mut self,
    ) -> Option<crate::handle::HandleRuntimeResolver<'call>> {
        self.handle_runtime.take()
    }

    pub(crate) fn resolve_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
    ) -> XllResult<crate::Handle<'call, T>> {
        let scope = self.scope.ok_or(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::HANDLE_SCOPE_MISSING,
        })?;
        self.handle_runtime
            .as_ref()
            .ok_or(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::HANDLE_NO_CONTEXT,
            })?
            .get()?
            .lookup(scope, token)
    }
}

/// Call-scoped argument conversion and formula-revision identity collection.
#[doc(hidden)]
pub struct ArgumentContext<'call> {
    pub(crate) call: CallContext<'call>,
    pub(crate) inputs: Option<InputFingerprintBuilder>,
}

impl<'call> ArgumentContext<'call> {
    pub fn for_return<R, A: crate::Addin>(
        runtime: &'call crate::Runtime<A>,
        scope: &'call CallScope<'call>,
    ) -> Self
    where
        R: ExcelReturn,
    {
        Self {
            call: CallContext::new(runtime, scope),
            inputs: R::USES_FORMULA_REVISION.then(InputFingerprintBuilder::new),
        }
    }

    pub(crate) fn take_handle_runtime(
        &mut self,
    ) -> Option<crate::handle::HandleRuntimeResolver<'call>> {
        self.call.take_handle_runtime()
    }

    pub fn finish(&mut self) -> Option<[u8; 32]> {
        self.inputs.take().map(|inputs| *inputs.finish().as_bytes())
    }
}

/// A generative lifetime token for one generated Excel call boundary.
#[doc(hidden)]
pub struct CallScope<'call> {
    callbacks: HostCallbackSession,
    handle_guard: crate::handle::HandleCallGuard,
    lifetime: PhantomData<&'call mut &'call ()>,
}

impl<'call> CallScope<'call> {
    pub(crate) fn new() -> Self {
        Self {
            callbacks: HostCallbackSession::new(),
            handle_guard: crate::handle::HandleCallGuard::new(),
            lifetime: PhantomData,
        }
    }

    pub(crate) fn callbacks(&self) -> &HostCallbackSession {
        &self.callbacks
    }

    pub(crate) fn handle_guard(&'call self) -> &'call crate::handle::HandleCallGuard {
        &self.handle_guard
    }
}

/// Runs an operation under a fresh lifetime that cannot escape in its result.
#[doc(hidden)]
pub fn with_excel_call_scope<R>(
    operation: impl for<'scope> FnOnce(&'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(&scope)
}

/// Runs an operation under a fresh call scope while borrowing existing state.
#[doc(hidden)]
pub(crate) fn with_excel_call_scope_and_state<S, R>(
    state: &S,
    operation: impl for<'scope> FnOnce(&'scope S, &'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(state, &scope)
}

/// Converts one raw Excel argument at the generated ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `XlValueRef::from_raw` for the duration of the
/// conversion.
#[doc(hidden)]
pub unsafe fn argument_from_raw<'call, T>(
    _scope: &'call CallScope<'call>,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(borrowed, argument, &CallContext::without_runtime(), None)
}

#[doc(hidden)]
pub unsafe fn argument_from_raw_with_context<'call, A, T>(
    _scope: &'call CallScope<'call>,
    runtime: &'call crate::Runtime<A>,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    A: crate::Addin,
    T: ExcelParameter<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(borrowed, argument, &CallContext::new(runtime, _scope), None)
}

/// Converts one raw Excel argument and records its framework identity.
#[doc(hidden)]
pub unsafe fn argument_from_raw_with_arguments<'call, T>(
    arguments: &mut ArgumentContext<'call>,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    if let Some(inputs) = &mut arguments.inputs {
        inputs.with_argument(argument, |identity| {
            T::decode(borrowed, argument, &arguments.call, Some(identity))
        })
    } else {
        T::decode(borrowed, argument, &arguments.call, None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum CellPresence {
    Value,
    Blank,
    Missing,
}

/// Reads only Excel's presence marker without converting the contained value.
#[doc(hidden)]
pub unsafe fn cell_presence_from_raw(
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<CellPresence> {
    // SAFETY: this function forwards its caller's raw-value contract.
    let value = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    Ok(match value.base_type() {
        XLTYPE_NIL => CellPresence::Blank,
        XLTYPE_MISSING => CellPresence::Missing,
        _ => CellPresence::Value,
    })
}

#[doc(hidden)]
pub fn assert_excel_parameter<'call, T: ExcelParameter<'call>>(_: &CallScope<'call>) {}

#[doc(hidden)]
pub fn assert_async_parameter<T>()
where
    T: for<'call> ExcelParameter<'call> + Send + 'static,
{
}
