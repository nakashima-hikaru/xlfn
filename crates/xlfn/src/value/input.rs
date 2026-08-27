//! Worksheet-input conversion and call-boundary state.

use crate::call::CallScope;
use crate::input_identity::{InputFingerprintBuilder, InputIdentityEncoder};
use crate::{XllError, XllResult};
use xlfn_sys::XLOPER12;

use super::{ExcelReturn, XlValueRef, XlValueType};

pub(crate) mod sealed {
    pub trait InputModeSealed {}

    pub trait ExcelParameterSealed<'call, M: super::InputMode> {}
}

/// Input conversion mode selected by the return type of the UDF.
#[doc(hidden)]
pub trait InputMode: sealed::InputModeSealed + Sized {
    type Identity;
    type Fingerprint;

    #[doc(hidden)]
    fn new_fingerprint(argument_count: usize) -> Self::Fingerprint;

    #[doc(hidden)]
    fn with_argument<R>(
        fingerprint: &mut Self::Fingerprint,
        index: usize,
        argument: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R>;

    #[doc(hidden)]
    fn finish(fingerprint: Self::Fingerprint) -> XllResult<Option<[u8; 32]>>;

    #[doc(hidden)]
    fn tag(identity: &mut Self::Identity, value: u8);

    #[doc(hidden)]
    fn bool(identity: &mut Self::Identity, value: bool);

    #[doc(hidden)]
    fn f64(identity: &mut Self::Identity, value: f64);

    #[doc(hidden)]
    fn i64(identity: &mut Self::Identity, value: i64);

    #[doc(hidden)]
    fn u64(identity: &mut Self::Identity, value: u64);

    #[doc(hidden)]
    fn string(identity: &mut Self::Identity, value: &str);
}

/// Plain worksheet conversion, without formula-revision identity recording.
#[doc(hidden)]
pub struct PlainInputMode;

/// Formula-revision worksheet conversion with semantic identity recording.
#[doc(hidden)]
pub struct FormulaInputMode;

impl sealed::InputModeSealed for PlainInputMode {}
impl sealed::InputModeSealed for FormulaInputMode {}

impl InputMode for PlainInputMode {
    type Identity = ();
    type Fingerprint = ();

    fn new_fingerprint(_: usize) -> Self::Fingerprint {}

    fn with_argument<R>(
        _: &mut Self::Fingerprint,
        _: usize,
        _: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R> {
        let mut identity = ();
        encode(&mut identity)
    }

    fn finish(_: Self::Fingerprint) -> XllResult<Option<[u8; 32]>> {
        Ok(None)
    }

    fn tag(_: &mut Self::Identity, _: u8) {}
    fn bool(_: &mut Self::Identity, _: bool) {}
    fn f64(_: &mut Self::Identity, _: f64) {}
    fn i64(_: &mut Self::Identity, _: i64) {}
    fn u64(_: &mut Self::Identity, _: u64) {}
    fn string(_: &mut Self::Identity, _: &str) {}
}

impl InputMode for FormulaInputMode {
    type Identity = InputIdentityEncoder;
    type Fingerprint = InputFingerprintBuilder;

    fn new_fingerprint(argument_count: usize) -> Self::Fingerprint {
        InputFingerprintBuilder::new(argument_count)
    }

    fn with_argument<R>(
        fingerprint: &mut Self::Fingerprint,
        index: usize,
        argument: &'static str,
        encode: impl FnOnce(&mut Self::Identity) -> XllResult<R>,
    ) -> XllResult<R> {
        fingerprint.with_argument(index, argument, encode)
    }

    fn finish(fingerprint: Self::Fingerprint) -> XllResult<Option<[u8; 32]>> {
        fingerprint
            .finish()
            .map(|fingerprint| Some(*fingerprint.as_bytes()))
    }

    fn tag(identity: &mut Self::Identity, value: u8) {
        identity.tag(value);
    }

    fn bool(identity: &mut Self::Identity, value: bool) {
        identity.bool(value);
    }

    fn f64(identity: &mut Self::Identity, value: f64) {
        identity.f64(value);
    }

    fn i64(identity: &mut Self::Identity, value: i64) {
        identity.i64(value);
    }

    fn u64(identity: &mut Self::Identity, value: u64) {
        identity.u64(value);
    }

    fn string(identity: &mut Self::Identity, value: &str) {
        identity.string(value);
    }
}

/// Converts a call-scoped Excel value into owned Rust data.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be converted from an Excel argument",
    label = "`{Self}` does not implement `FromExcel`",
    note = "implement `FromExcel` for this argument type or use a supported argument type"
)]
pub trait FromExcel<'call>: Sized {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;
}

/// Encodes the semantic value observed by a formula-revision UDF.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be hashed as a formula-revision input identity",
    label = "`{Self}` does not implement `ExcelInputIdentity`",
    note = "implement `ExcelInputIdentity` for `{Self}` to support formula revision tracking"
)]
pub trait ExcelInputIdentity {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder);
}

/// Framework-side argument dispatch used by generated ABI wrappers.
#[doc(hidden)]
pub trait ExcelParameter<'call, M: InputMode>:
    sealed::ExcelParameterSealed<'call, M> + Sized
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self>;

    fn encode_decoded(&self, identity: &mut M::Identity);
}

impl<'call, T: FromExcel<'call>> sealed::ExcelParameterSealed<'call, PlainInputMode> for T {}

impl<'call, T: FromExcel<'call>> ExcelParameter<'call, PlainInputMode> for T {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        _: &mut (),
    ) -> XllResult<Self> {
        T::from_excel(value, argument)
    }

    fn encode_decoded(&self, _: &mut ()) {}
}

impl<'call, T> sealed::ExcelParameterSealed<'call, FormulaInputMode> for T where
    T: FromExcel<'call> + ExcelInputIdentity
{
}

impl<'call, T> ExcelParameter<'call, FormulaInputMode> for T
where
    T: FromExcel<'call> + ExcelInputIdentity,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        let result = T::from_excel(value, argument)?;
        result.encode_input_identity(identity);
        Ok(result)
    }

    fn encode_decoded(&self, identity: &mut InputIdentityEncoder) {
        self.encode_input_identity(identity);
    }
}

/// Runtime services that travel together through one Excel-visible call.
#[cfg(feature = "handles")]
pub(crate) struct HandleCallAccess<'call> {
    pub(crate) runtime: crate::handle::FormulaHandleServiceResolver<'call>,
    pub(crate) scope: &'call CallScope<'call>,
}

/// Runtime services available to one admitted Excel-visible call.
///
/// Generation services are independent from the optional formula-handle
/// capability. Keeping them as separate fields means RTD access does not
/// acquire a handle resolver, and a core-only build has no handle access path.
struct RuntimeCallAccess<'call> {
    scope: &'call CallScope<'call>,
    #[cfg(feature = "handles")]
    handles: crate::handle::FormulaHandleServiceResolver<'call>,
    #[cfg(feature = "rtd")]
    rtd: crate::rtd::RtdGenerationAccess<'call>,
}

/// Runtime services available while converting one Excel-visible argument.
#[doc(hidden)]
pub struct CallContext<'call> {
    access: CallAccess<'call>,
}

/// The call either has plain conversion access or the runtime services for an
/// admitted generation.
enum CallAccess<'call> {
    Plain(&'call CallScope<'call>),
    Runtime(RuntimeCallAccess<'call>),
    #[cfg(all(test, feature = "handles"))]
    HandleOnly {
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
    },
}

impl<'call> CallContext<'call> {
    pub(crate) fn plain(scope: &'call CallScope<'call>) -> Self {
        Self {
            access: CallAccess::Plain(scope),
        }
    }

    pub(crate) fn with_call<A: crate::Addin>(
        call: &'call crate::runtime::CallGuard<'_, A>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        #[cfg(all(not(feature = "handles"), not(feature = "rtd")))]
        let _ = call;
        Self {
            access: CallAccess::Runtime(RuntimeCallAccess {
                scope,
                #[cfg(feature = "handles")]
                handles: call.handle_call_access(),
                #[cfg(feature = "rtd")]
                rtd: call.rtd_call_access(),
            }),
        }
    }

    #[cfg(all(test, feature = "handles"))]
    pub(crate) fn from_handle_access(
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
    ) -> Self {
        Self {
            access: CallAccess::HandleOnly { scope, handles },
        }
    }

    #[cfg(test)]
    pub(crate) fn from_scope(scope: &'call CallScope<'call>) -> Self {
        Self {
            access: CallAccess::Plain(scope),
        }
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn rtd_access(&self) -> crate::rtd::RtdGenerationAccess<'call> {
        match &self.access {
            CallAccess::Runtime(access) => access.rtd,
            CallAccess::Plain(_) => {
                panic!("plain conversion context has no RTD access")
            }
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { .. } => {
                panic!("handle-only conversion context has no RTD access")
            }
        }
    }

    pub(crate) fn scratch(&self) -> &'call crate::call::CallScratch {
        self.scope().scratch()
    }

    fn scope(&self) -> &'call CallScope<'call> {
        match &self.access {
            CallAccess::Plain(scope) => scope,
            CallAccess::Runtime(access) => access.scope,
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { scope, .. } => scope,
        }
    }

    #[cfg(feature = "handles")]
    pub(crate) fn take_handle_access(&mut self) -> Option<HandleCallAccess<'call>> {
        let scope = match &self.access {
            CallAccess::Plain(scope) => *scope,
            CallAccess::Runtime(access) => access.scope,
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { scope, .. } => *scope,
        };
        match std::mem::replace(&mut self.access, CallAccess::Plain(scope)) {
            CallAccess::Runtime(access) => Some(HandleCallAccess {
                runtime: access.handles,
                scope: access.scope,
            }),
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { scope, handles } => Some(HandleCallAccess {
                runtime: handles,
                scope,
            }),
            CallAccess::Plain(_) => None,
        }
    }

    #[cfg(feature = "handles")]
    pub(crate) fn resolve_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
    ) -> XllResult<crate::handle::Handle<'call, T>> {
        let (handles, scope) = match &self.access {
            CallAccess::Runtime(access) => (&access.handles, access.scope),
            #[cfg(all(test, feature = "handles"))]
            CallAccess::HandleOnly { handles, scope } => (handles, *scope),
            CallAccess::Plain(_) => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_NO_CONTEXT,
                });
            }
        };
        handles.get()?.lookup(scope, token)
    }
}

/// Call-scoped argument conversion and formula-revision identity collection.
#[doc(hidden)]
pub struct ArgumentContext<'call, M: InputMode> {
    pub(crate) call: CallContext<'call>,
    pub(crate) inputs: Option<M::Fingerprint>,
}

impl<'call, M: InputMode> ArgumentContext<'call, M> {
    pub fn new<A: crate::Addin>(
        call: &'call crate::runtime::CallGuard<'_, A>,
        scope: &'call CallScope<'call>,
        argument_count: usize,
    ) -> Self {
        Self {
            call: CallContext::with_call(call, scope),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_scope(scope: &'call CallScope<'call>, argument_count: usize) -> Self {
        Self {
            call: CallContext::from_scope(scope),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(all(test, feature = "handles"))]
    pub(crate) fn from_handle_access(
        scope: &'call CallScope<'call>,
        handles: crate::handle::FormulaHandleServiceResolver<'call>,
        argument_count: usize,
    ) -> Self {
        Self {
            call: CallContext::from_handle_access(scope, handles),
            inputs: Some(M::new_fingerprint(argument_count)),
        }
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn rtd_access(&self) -> crate::rtd::RtdGenerationAccess<'call> {
        self.call.rtd_access()
    }

    #[cfg(feature = "handles")]
    pub(crate) fn take_handle_access(&mut self) -> HandleCallAccess<'call> {
        self.call
            .take_handle_access()
            .expect("formula argument context must retain handle access")
    }

    pub fn finish(&mut self) -> XllResult<Option<[u8; 32]>> {
        self.inputs.take().map_or(Ok(None), M::finish)
    }

    pub(crate) fn decode<T>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: XlValueRef<'call>,
    ) -> XllResult<T>
    where
        T: ExcelParameter<'call, M>,
    {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        let call = &self.call;
        M::with_argument(fingerprint, index, argument, |identity| {
            T::decode(value, argument, call, identity)
        })
    }

    pub(crate) fn record_decoded<T>(
        &mut self,
        index: usize,
        argument: &'static str,
        value: &T,
    ) -> XllResult<()>
    where
        T: ExcelParameter<'call, M>,
    {
        let fingerprint = self.inputs.as_mut().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
        })?;
        M::with_argument(fingerprint, index, argument, |identity| {
            T::encode_decoded(value, identity);
            Ok(())
        })
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
    T: ExcelParameter<'call, PlainInputMode>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(borrowed, argument, &CallContext::plain(scope), &mut ())
}

#[doc(hidden)]
#[cfg(all(test, feature = "handles"))]
pub(crate) unsafe fn argument_from_raw_with_context<'call, T>(
    scope: &'call CallScope<'call>,
    slot: &'call crate::handle::FormulaHandleServiceSlot,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call, PlainInputMode>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::decode(
        borrowed,
        argument,
        &CallContext::from_handle_access(
            scope,
            crate::handle::FormulaHandleServiceResolver::new(slot),
        ),
        &mut (),
    )
}

/// Converts one raw Excel argument and records its framework identity.
#[doc(hidden)]
pub unsafe fn argument_from_raw_with_arguments<'call, M, T>(
    arguments: &mut ArgumentContext<'call, M>,
    index: usize,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    arguments.decode(index, argument, borrowed)
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
    Ok(match value.value_type() {
        XlValueType::Nil => CellPresence::Blank,
        XlValueType::Missing => CellPresence::Missing,
        _ => CellPresence::Value,
    })
}

#[doc(hidden)]
pub fn assert_async_parameter<R, T>()
where
    R: ExcelReturn,
    T: for<'call> ExcelParameter<'call, R::InputMode> + Send + 'static,
{
}
