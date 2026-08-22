//! Worksheet-input conversion and call-boundary state.

use crate::call::CallScope;
use crate::input_identity::{InputFingerprintBuilder, InputIdentityEncoder};
use crate::{XllError, XllResult};
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

/// Runtime services that travel together through one Excel-visible call.
pub(crate) struct HandleCallAccess<'call> {
    pub(crate) runtime: crate::handle::HandleRuntimeResolver<'call>,
    pub(crate) scope: &'call CallScope<'call>,
}

/// Runtime services available while converting one Excel-visible argument.
#[doc(hidden)]
pub struct CallContext<'call> {
    access: CallAccess<'call>,
}

/// The call either has plain conversion access or the complete handle access
/// bundle. Keeping the scope and resolver in one variant prevents a malformed
/// half-configured handle context from being constructed.
enum CallAccess<'call> {
    Plain(&'call CallScope<'call>),
    Handles(HandleCallAccess<'call>),
}

impl<'call> CallContext<'call> {
    pub(crate) fn plain(scope: &'call CallScope<'call>) -> Self {
        Self {
            access: CallAccess::Plain(scope),
        }
    }

    pub(crate) fn with_runtime<A: crate::Addin>(
        runtime: &'call crate::runtime::Runtime<A>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        Self {
            access: CallAccess::Handles(HandleCallAccess {
                runtime: crate::handle::HandleRuntimeResolver::new(runtime.handle_runtime_slot()),
                scope,
            }),
        }
    }

    pub(crate) fn scratch(&self) -> &'call crate::call::CallScratch {
        self.scope().scratch()
    }

    fn scope(&self) -> &'call CallScope<'call> {
        match &self.access {
            CallAccess::Plain(scope) => scope,
            CallAccess::Handles(access) => access.scope,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_access(
        scope: &'call CallScope<'call>,
        handle_runtime: Option<crate::handle::HandleRuntimeResolver<'call>>,
    ) -> Self {
        Self {
            access: match handle_runtime {
                Some(runtime) => CallAccess::Handles(HandleCallAccess { runtime, scope }),
                None => CallAccess::Plain(scope),
            },
        }
    }

    pub(crate) fn take_handle_access(&mut self) -> Option<HandleCallAccess<'call>> {
        let scope = self.scope();
        match std::mem::replace(&mut self.access, CallAccess::Plain(scope)) {
            CallAccess::Handles(access) => Some(access),
            CallAccess::Plain(_) => None,
        }
    }

    pub(crate) fn resolve_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
    ) -> XllResult<crate::handle::Handle<'call, T>> {
        let CallAccess::Handles(access) = &self.access else {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_NO_CONTEXT,
            });
        };
        access.runtime.get()?.lookup(access.scope, token)
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
        runtime: &'call crate::runtime::Runtime<A>,
        scope: &'call CallScope<'call>,
    ) -> Self
    where
        R: ExcelReturn,
    {
        Self {
            call: CallContext::with_runtime(runtime, scope),
            inputs: R::USES_FORMULA_REVISION.then(InputFingerprintBuilder::new),
        }
    }

    pub(crate) fn take_handle_access(&mut self) -> HandleCallAccess<'call> {
        self.call
            .take_handle_access()
            .expect("formula argument context must retain handle access")
    }

    pub fn finish(&mut self) -> Option<[u8; 32]> {
        self.inputs.take().map(|inputs| *inputs.finish().as_bytes())
    }
}

/// Converts one raw Excel argument at the generated ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `XlValueRef::from_raw` for the duration of the
/// conversion.
#[doc(hidden)]
pub unsafe fn argument_from_raw<'call, T>(
    scope: &'call CallScope<'call>,
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
    T::decode(borrowed, argument, &CallContext::plain(scope), None)
}

#[doc(hidden)]
pub unsafe fn argument_from_raw_with_context<'call, A, T>(
    scope: &'call CallScope<'call>,
    runtime: &'call crate::runtime::Runtime<A>,
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
    T::decode(
        borrowed,
        argument,
        &CallContext::with_runtime(runtime, scope),
        None,
    )
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
pub fn assert_async_parameter<T>()
where
    T: for<'call> ExcelParameter<'call> + Send + 'static,
{
}
