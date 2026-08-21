use crate::host_callback::HostCallbackSession;
use crate::input_identity::{InputFingerprintBuilder, InputIdentityEncoder};
use crate::return_array::{XlArrayBuilder, XlArrayOutput};
use crate::{
    DomainErrorCode, ExcelError, InputError, IntoXllError, ReturnContext, Shape, XllError,
    XllResult,
};
use std::marker::PhantomData;
#[cfg(test)]
use xlfn_sys::XLOPER12Array;
use xlfn_sys::{
    XLOPER12, XLTYPE_BOOL, XLTYPE_ERR, XLTYPE_INT, XLTYPE_MISSING, XLTYPE_MULTI, XLTYPE_NIL,
    XLTYPE_NUM, XLTYPE_STR,
};

/// Borrowed call-scoped views used while converting one worksheet call.
pub mod borrowed;
/// Excel serial-date policy and value types.
pub mod date;
/// Internal semantic identity support used by generated input conversion.
#[doc(hidden)]
pub mod identity;
/// Input conversion traits and presence/default handling.
pub mod input;
/// Owned rectangular and bounded collection values.
pub mod matrix;
/// Output conversion traits and return-cell representations.
pub mod output;
/// Raw, borrowed views over Excel's XLOPER12 input representation.
pub mod raw;

pub use date::{ExcelDateSystem, ExcelSerialDate};
pub(crate) use matrix::validate_matrix_dimensions;
pub use matrix::{BoundedVarArgs, Column, Matrix, Row};
pub(crate) use raw::{GridView, encode_raw_value};
pub use raw::{XlArrayRef, XlStrRef, XlValueRef};

const MAX_UTF16_UNITS: usize = 32_767;
const EXCEL_MAX_ROWS: usize = 1_048_576;
const EXCEL_MAX_COLUMNS: usize = 16_384;
#[cfg(target_pointer_width = "32")]
const MAX_ARRAY_ELEMENTS: usize = 1_000_000;
#[cfg(not(target_pointer_width = "32"))]
const MAX_ARRAY_ELEMENTS: usize = 4_000_000;
#[cfg(target_pointer_width = "32")]
pub(crate) const MAX_ARRAY_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_pointer_width = "32"))]
pub(crate) const MAX_ARRAY_BYTES: usize = 256 * 1024 * 1024;

/// Converts a call-scoped Excel value into owned Rust data.
///
/// The input lifetime is deliberately anonymous: an implementation cannot
/// choose it or store a reference to Excel-owned memory in `Self`.
///
/// ```compile_fail
/// use xlfn::{FromExcel, XlValueRef, XllResult};
/// use xlfn_sys::XLOPER12;
///
/// struct Escaped(&'static XLOPER12);
///
/// impl<'call> FromExcel<'call> for Escaped {
///     fn from_excel(
///         value: XlValueRef<'call>,
///         _: &'static str,
///     ) -> XllResult<Self> {
///         Ok(Self(value.raw()))
///     }
/// }
/// ```
pub trait FromExcel<'call>: Sized {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;

    /// Internal context-aware hook used only for framework-owned special
    /// values such as handles.
    #[doc(hidden)]
    fn from_excel_with_context(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
        Self::from_excel(value, argument)
    }

    /// Internal identity hook for framework-owned formula revisions.
    #[doc(hidden)]
    fn encode_identity(&self, _encoder: &mut InputIdentityEncoder) {}

    /// Internal single-pass conversion hook. Ordinary custom conversions use
    /// the raw Excel representation as their fallback identity; framework
    /// types override this to preserve semantic identities.
    #[doc(hidden)]
    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let result = Self::from_excel_with_context(value, argument, context)?;
        if let Some(identity) = identity {
            encode_raw_value(value, false, identity);
        }
        Ok(result)
    }
}

/// Framework-side argument dispatch.
///
/// This bridge is public only because generated proc-macro code is compiled
/// in the add-in crate. It is re-exported by `xlfn::macro_support`, not by the
/// normal `xlfn` value API.
#[doc(hidden)]
pub trait ExcelParameter<'call>: Sized {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self>;

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder);

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let result = Self::from_excel(value, argument, context)?;
        if let Some(identity) = identity {
            result.encode_identity(identity);
        }
        Ok(result)
    }
}

impl<'call, T: FromExcel<'call>> ExcelParameter<'call> for T {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        T::from_excel_with_identity(value, argument, context, None)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        T::encode_identity(self, encoder);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        T::from_excel_with_identity(value, argument, context, identity)
    }
}

/// Runtime services available while converting one Excel-visible argument.
///
/// The handle runtime is acquired lazily so ordinary scalar conversions do not
/// initialize handle registry state.
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

/// Call-scoped argument conversion and semantic identity collection.
///
/// Conversion always runs before memoization lookup. The identity builder is
/// allocated only when the return type can publish a formula-owned revision;
/// ordinary UDFs therefore pay no semantic fingerprinting cost.
#[doc(hidden)]
pub struct ArgumentContext<'call> {
    call: CallContext<'call>,
    inputs: Option<InputFingerprintBuilder>,
}

impl<'call> ArgumentContext<'call> {
    #[doc(hidden)]
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

    #[doc(hidden)]
    pub fn record_value<T: ExcelParameter<'call>>(
        &mut self,
        argument: &'static str,
        value: &T,
    ) -> XllResult<()> {
        if let Some(inputs) = &mut self.inputs {
            inputs.with_argument(argument, |encoder| {
                value.encode_identity(encoder);
                Ok(())
            })?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn finish(&mut self) -> Option<[u8; 32]> {
        self.inputs.take().map(|inputs| *inputs.finish().as_bytes())
    }
}

/// Converts an ordinary Rust value into a semantic Excel cell.
///
/// This is the public conversion extension point for scalar worksheet
/// outputs and for cells written through `xlfn::unstable::output::XlArrayBuilder`. Runtime
/// ownership, handle publication, and array allocation remain internal to the
/// return dispatcher.
pub trait IntoExcel {
    fn into_excel(self) -> XllResult<ExcelCellOutput>;
}

/// Framework-side return dispatch.
///
/// This bridge is public only for generated proc-macro code and is exposed by
/// `xlfn::macro_support`, not by the normal `xlfn` value API.
#[doc(hidden)]
pub trait ExcelReturn: Sized {
    /// Whether this return path publishes a formula revision.
    const USES_FORMULA_REVISION: bool = false;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput>;

    #[doc(hidden)]
    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<ExcelOutput> {
        operation()?.into_excel(context)
    }
}

/// Return values supported by ordinary main-thread worksheet functions.
#[doc(hidden)]
pub trait MainThreadReturn: ExcelReturn {}

/// Return values supported by Excel multi-threaded recalculation.
#[doc(hidden)]
pub trait ThreadSafeReturn: ExcelReturn {}

/// Return values supported by macro-sheet functions.
#[doc(hidden)]
pub trait MacroSheetReturn: ExcelReturn {}

/// Return values supported by native asynchronous functions.
#[doc(hidden)]
pub trait AsyncReturn: ExcelReturn {}

/// Return values supported by volatile functions.
#[doc(hidden)]
pub trait VolatileReturn: ExcelReturn {}

impl<T: IntoExcel> ExcelReturn for T {
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        IntoExcel::into_excel(self).map(ExcelOutput::Scalar)
    }
}

impl<T: IntoExcel> MainThreadReturn for T {}
impl<T: IntoExcel> ThreadSafeReturn for T {}
impl<T: IntoExcel> MacroSheetReturn for T {}
impl<T: IntoExcel> AsyncReturn for T {}
impl<T: IntoExcel> VolatileReturn for T {}

impl<T, E> ExcelReturn for Result<T, E>
where
    T: ExcelReturn,
    E: IntoXllError,
{
    const USES_FORMULA_REVISION: bool = T::USES_FORMULA_REVISION;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        self.map_err(IntoXllError::into_xll_error)?
            .into_excel(context)
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<ExcelOutput> {
        T::invoke(context, || {
            operation()?.map_err(IntoXllError::into_xll_error)
        })
    }
}

impl<T, E> MainThreadReturn for Result<T, E>
where
    T: MainThreadReturn,
    E: IntoXllError,
{
}

impl<T, E> ThreadSafeReturn for Result<T, E>
where
    T: ThreadSafeReturn,
    E: IntoXllError,
{
}

impl<T, E> MacroSheetReturn for Result<T, E>
where
    T: MacroSheetReturn,
    E: IntoXllError,
{
}

impl<T, E> AsyncReturn for Result<T, E>
where
    T: AsyncReturn,
    E: IntoXllError,
{
}

impl<T, E> VolatileReturn for Result<T, E>
where
    T: VolatileReturn,
    E: IntoXllError,
{
}

#[doc(hidden)]
pub fn assert_excel_parameter<'call, T: ExcelParameter<'call>>(_: &CallScope<'call>) {}

#[doc(hidden)]
#[doc(hidden)]
pub fn assert_async_parameter<T>()
where
    T: for<'call> ExcelParameter<'call> + Send + 'static,
{
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

/// Runs an operation under a fresh call scope while borrowing existing state
/// for exactly the same callback lifetime.
pub(crate) fn with_excel_call_scope_and_state<S, R>(
    state: &S,
    operation: impl for<'scope> FnOnce(&'scope S, &'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope::new();
    operation(state, &scope)
}

#[doc(hidden)]
pub fn assert_main_thread_return<T: MainThreadReturn>() {}

#[doc(hidden)]
pub fn assert_thread_safe_return<T: ThreadSafeReturn>() {}

#[doc(hidden)]
pub fn assert_macro_sheet_return<T: MacroSheetReturn>() {}

#[doc(hidden)]
pub fn assert_async_return<T: AsyncReturn>() {}

#[doc(hidden)]
pub fn assert_volatile_return<T: VolatileReturn>() {}

/// Converts one raw Excel argument at the generated ABI boundary.
///
/// # Safety
///
/// The pointer must satisfy `XlValueRef::from_raw` for the duration of
/// the conversion.
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
    T::from_excel(borrowed, argument, &CallContext::without_runtime())
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
    T::from_excel(borrowed, argument, &CallContext::new(runtime, _scope))
}

#[doc(hidden)]
/// Converts one raw Excel argument and records it in the generated argument
/// context.
///
/// # Safety
/// The pointer must satisfy `XlValueRef::from_raw` for the duration of the
/// conversion.
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
    let call = &arguments.call;
    if let Some(inputs) = &mut arguments.inputs {
        inputs.with_argument(argument, |identity| {
            T::from_excel_with_identity(borrowed, argument, call, Some(identity))
        })
    } else {
        T::from_excel_with_identity(borrowed, argument, call, None)
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
///
/// # Safety
///
/// `raw` must satisfy `XlValueRef::from_raw` for this call.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExcelErrorValue(pub ExcelError);

#[derive(Clone, Debug, PartialEq)]
pub enum ExcelCellValue {
    Number(f64),
    Boolean(bool),
    String(String),
    Error(ExcelError),
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExcelValue {
    Scalar(ExcelCellValue),
    Missing,
    Array(Matrix<ExcelCellValue>),
}

#[repr(u8)]
enum ExcelCellValueKind {
    Number = 1,
    Boolean = 2,
    String = 3,
    Error = 4,
    Blank = 5,
}

#[repr(u8)]
enum ExcelValueKind {
    Scalar = 1,
    Missing = 2,
    Array = 3,
}

/// A single worksheet cell in the final semantic return representation.
///
/// Unlike [`ExcelCellValue`], this type cannot represent an omitted or blank
/// cell. Use an explicit empty string or [`ExcelError::NotAvailable`] when that
/// is the intended worksheet result.
#[derive(Clone, Debug, PartialEq)]
pub enum ExcelCellOutput {
    Number(f64),
    Boolean(bool),
    String(String),
    Error(ExcelError),
}

/// The complete semantic representation of a worksheet return value.
///
/// Array returns contain an already encoded [`XlArrayOutput`] buffer; input
/// values and ABI transport forms never appear as variants here.
#[doc(hidden)]
pub enum ExcelOutput {
    Scalar(ExcelCellOutput),
    Array(XlArrayOutput),
}

/// An input-only distinction between an omitted and a blank Excel argument.
///
/// Excel does not preserve these meanings for UDF return values: both are
/// displayed as numeric zero. Return an explicit value, empty string, or
/// `ExcelErrorValue` instead.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionalExcelValue<T> {
    Missing,
    Blank,
    Value(T),
}

#[repr(u8)]
enum OptionalValueKind {
    Missing = 0,
    Blank = 1,
    Value = 2,
}

fn convert_with_semantic_identity<T>(
    identity: Option<&mut InputIdentityEncoder>,
    convert: impl FnOnce() -> XllResult<T>,
    encode: impl FnOnce(&T, &mut InputIdentityEncoder),
) -> XllResult<T> {
    let value = convert()?;

    if let Some(identity) = identity {
        encode(&value, identity);
    }

    Ok(value)
}

impl<'call> FromExcel<'call> for f64 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let number = match value.base_type() {
            // SAFETY: The root type selects the corresponding union member.
            XLTYPE_NUM => unsafe { value.raw.value.number },
            // SAFETY: The root type selects the corresponding union member.
            XLTYPE_INT => (unsafe { value.raw.value.integer }) as f64,
            _ => return Err(value.wrong_type(argument, "number")),
        };
        if !number.is_finite() {
            return Err(XllError::input(argument, InputError::NonFinite));
        }
        Ok(number)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(*self);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.f64(*value),
        )
    }
}

impl<'call> FromExcel<'call> for bool {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.base_type() != XLTYPE_BOOL {
            return Err(value.wrong_type(argument, "boolean"));
        }
        // SAFETY: XLTYPE_BOOL selects the boolean member.
        Ok(unsafe { value.raw.value.boolean } != 0)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.bool(*self);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.bool(*value),
        )
    }
}

fn number_to_integer<T>(
    number: f64,
    argument: &'static str,
    minimum: f64,
    maximum: f64,
    convert: impl FnOnce(f64) -> T,
) -> XllResult<T> {
    if !number.is_finite() {
        return Err(XllError::input(argument, InputError::NonFinite));
    }
    if number.fract() != 0.0 {
        return Err(XllError::input(argument, InputError::NotInteger));
    }
    if number < minimum || number > maximum {
        return Err(XllError::input(argument, InputError::NumericOverflow));
    }
    Ok(convert(number))
}

impl<'call> FromExcel<'call> for i32 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.base_type() {
            // SAFETY: XLTYPE_INT selects the integer member.
            XLTYPE_INT => Ok(unsafe { value.raw.value.integer }),
            // SAFETY: XLTYPE_NUM selects the number member.
            XLTYPE_NUM => number_to_integer(
                unsafe { value.raw.value.number },
                argument,
                i32::MIN as f64,
                i32::MAX as f64,
                |number| number as i32,
            ),
            _ => Err(value.wrong_type(argument, "integer")),
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(i64::from(*self));
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.i64(i64::from(*value)),
        )
    }
}

impl<'call> FromExcel<'call> for i64 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.base_type() {
            // SAFETY: XLTYPE_INT selects the integer member.
            XLTYPE_INT => Ok((unsafe { value.raw.value.integer }) as i64),
            // Excel doubles can represent every integer only through 2^53.
            // SAFETY: XLTYPE_NUM selects the number member.
            XLTYPE_NUM => number_to_integer(
                unsafe { value.raw.value.number },
                argument,
                -((1_u64 << 53) as f64),
                (1_u64 << 53) as f64,
                |number| number as i64,
            ),
            _ => Err(value.wrong_type(argument, "integer")),
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(*self);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.i64(*value),
        )
    }
}

impl<'call> FromExcel<'call> for String {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        String::from_utf16(value.utf16(argument)?)
            .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.string(self);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.string(value),
        )
    }
}

impl<'call, T> FromExcel<'call> for crate::Handle<'call, T>
where
    T: crate::handle::ExcelHandleObject,
{
    fn from_excel(_value: XlValueRef<'call>, _argument: &'static str) -> XllResult<Self> {
        Err(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::HANDLE_NO_CONTEXT,
        })
    }

    fn from_excel_with_context(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let token = <String as FromExcel>::from_excel(value, argument)?;
        context.resolve_handle(&token)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.object.id.0.0);
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let handle = Self::from_excel_with_context(value, argument, context)?;
        if let Some(identity) = identity {
            identity.u64(handle.object.id.0.0);
        }
        Ok(handle)
    }
}

impl<'call, T> FromExcel<'call> for crate::handle::AsyncHandle<T>
where
    T: crate::handle::ExcelHandleObject,
{
    fn from_excel(_value: XlValueRef<'call>, _argument: &'static str) -> XllResult<Self> {
        Err(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::HANDLE_NO_CONTEXT,
        })
    }

    fn from_excel_with_context(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let token = <String as FromExcel>::from_excel(value, argument)?;
        context.resolve_handle::<T>(&token)?.into_async()
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.object_id());
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let handle = Self::from_excel_with_context(value, argument, context)?;
        if let Some(identity) = identity {
            identity.u64(handle.object_id());
        }
        Ok(handle)
    }
}

impl<'call> FromExcel<'call> for ExcelErrorValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.base_type() != XLTYPE_ERR {
            return Err(value.wrong_type(argument, "Excel error"));
        }
        // SAFETY: XLTYPE_ERR selects the error member.
        let code = unsafe { value.raw.value.error };
        ExcelError::from_code(code)
            .map(Self)
            .ok_or_else(|| XllError::input(argument, InputError::Malformed("unknown error code")))
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(i64::from(self.0.code()));
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| encoder.i64(i64::from(value.0.code())),
        )
    }
}

impl<'call, T> FromExcel<'call> for OptionalExcelValue<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING => Ok(Self::Missing),
            XLTYPE_NIL => Ok(Self::Blank),
            _ => T::from_excel(value, argument).map(Self::Value),
        }
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING => {
                if let Some(identity) = identity.as_mut() {
                    identity.tag(OptionalValueKind::Missing as u8);
                }
                Ok(Self::Missing)
            }
            XLTYPE_NIL => {
                if let Some(identity) = identity.as_mut() {
                    identity.tag(OptionalValueKind::Blank as u8);
                }
                Ok(Self::Blank)
            }
            _ => {
                if let Some(identity) = identity.as_mut() {
                    identity.tag(OptionalValueKind::Value as u8);
                    <T as FromExcel>::from_excel_with_identity(
                        value,
                        argument,
                        context,
                        Some(&mut **identity),
                    )
                    .map(Self::Value)
                } else {
                    <T as FromExcel>::from_excel_with_identity(value, argument, context, None)
                        .map(Self::Value)
                }
            }
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Missing => encoder.tag(OptionalValueKind::Missing as u8),
            Self::Blank => encoder.tag(OptionalValueKind::Blank as u8),
            Self::Value(value) => {
                encoder.tag(OptionalValueKind::Value as u8);
                value.encode_identity(encoder);
            }
        }
    }
}

impl<'call, T> FromExcel<'call> for Option<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING | XLTYPE_NIL => Ok(None),
            _ => T::from_excel(value, argument).map(Some),
        }
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING | XLTYPE_NIL => {
                if let Some(identity) = identity.as_mut() {
                    identity.bool(false);
                }
                Ok(None)
            }
            _ => {
                if let Some(identity) = identity.as_mut() {
                    identity.bool(true);
                    <T as FromExcel>::from_excel_with_identity(
                        value,
                        argument,
                        context,
                        Some(&mut **identity),
                    )
                    .map(Some)
                } else {
                    <T as FromExcel>::from_excel_with_identity(value, argument, context, None)
                        .map(Some)
                }
            }
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            None => encoder.bool(false),
            Some(value) => {
                encoder.bool(true);
                value.encode_identity(encoder);
            }
        }
    }
}

impl<'call> FromExcel<'call> for ExcelSerialDate {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        Self::new(
            <f64 as FromExcel>::from_excel(value, argument)?,
            ExcelDateSystem::Workbook,
        )
        .map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(self.serial);
        encoder.tag(self.date_system.identity_tag());
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
        identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        convert_with_semantic_identity(
            identity,
            || <Self as FromExcel>::from_excel(value, argument),
            |value, encoder| {
                encoder.f64(value.serial);
                encoder.tag(value.date_system.identity_tag());
            },
        )
    }
}

fn convert_grid_elements_with_identity<'call, T>(
    grid: &GridView<'call>,
    argument: &'static str,
    context: &CallContext<'call>,
    mut identity: Option<&mut InputIdentityEncoder>,
) -> XllResult<Vec<T>>
where
    T: FromExcel<'call>,
{
    let (rows, columns) = grid.shape();
    let element_count = rows * columns;
    let output_bytes = element_count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("output byte-size overflow"))
        })?;
    let mut referenced_bytes = element_count
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|bytes| bytes.checked_add(output_bytes))
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array byte-size overflow"))
        })?;
    if referenced_bytes > MAX_ARRAY_BYTES {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: MAX_ARRAY_BYTES,
                actual: referenced_bytes,
            },
        ));
    }

    let mut data = Vec::with_capacity(element_count);
    for index in 0..element_count {
        let element = grid.element(index)?;
        if element.base_type() == XLTYPE_MULTI {
            return Err(XllError::input(
                argument,
                InputError::Malformed("nested arrays are not supported"),
            ));
        }
        if element.base_type() == XLTYPE_STR {
            let string_bytes = element
                .utf16(argument)?
                .len()
                // Two bytes for the Excel source plus up to three bytes
                // per UTF-16 unit for the owned UTF-8 conversion.
                .checked_mul(std::mem::size_of::<u16>() + 3)
                .ok_or_else(|| {
                    XllError::input(
                        argument,
                        InputError::Malformed("array string byte-size overflow"),
                    )
                })?;
            referenced_bytes = referenced_bytes.checked_add(string_bytes).ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
            if referenced_bytes > MAX_ARRAY_BYTES {
                return Err(XllError::input(
                    argument,
                    InputError::TooLarge {
                        limit: MAX_ARRAY_BYTES,
                        actual: referenced_bytes,
                    },
                ));
            }
        }
        let converted = match identity.as_mut() {
            Some(identity) => <T as FromExcel>::from_excel_with_identity(
                element,
                argument,
                context,
                Some(&mut **identity),
            )?,
            None => <T as FromExcel>::from_excel_with_identity(element, argument, context, None)?,
        };
        data.push(converted);
    }
    Ok(data)
}

impl<'call, T> FromExcel<'call> for Matrix<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if let Some(identity) = identity.as_deref_mut() {
            identity.u64(rows as u64);
            identity.u64(columns as u64);
        }
        let data = convert_grid_elements_with_identity(&grid, argument, context, identity)?;
        Matrix::new(rows, columns, data)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.rows as u64);
        encoder.u64(self.columns as u64);
        for value in &self.data {
            value.encode_identity(encoder);
        }
    }
}

impl<'call, T> FromExcel<'call> for Vec<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 && columns != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: rows * columns,
                },
                actual: Shape { rows, columns },
            });
        }
        let len = rows * columns;
        if let Some(identity) = identity.as_deref_mut() {
            identity.u64(len as u64);
        }
        convert_grid_elements_with_identity(&grid, argument, context, identity)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.len() as u64);
        for value in self {
            value.encode_identity(encoder);
        }
    }
}

impl<'call, T, const MAX: usize> FromExcel<'call> for BoundedVarArgs<T, MAX>
where
    T: FromExcel<'call>,
{
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        if MAX == 0 {
            return Err(XllError::input(
                argument,
                InputError::Malformed("bounded varargs maximum must be non-zero"),
            ));
        }
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 && columns != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: rows * columns,
                },
                actual: Shape { rows, columns },
            });
        }
        let actual = rows * columns;
        if actual > MAX {
            return Err(XllError::input(
                argument,
                InputError::TooLarge { limit: MAX, actual },
            ));
        }
        if let Some(identity) = identity.as_deref_mut() {
            identity.u64(actual as u64);
        }
        let elements = convert_grid_elements_with_identity(&grid, argument, context, identity)?;
        Self::new(elements).map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.0.len() as u64);
        for value in &self.0 {
            value.encode_identity(encoder);
        }
    }
}

impl<'call, T: FromExcel<'call>> FromExcel<'call> for Row<T> {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 {
            return Err(XllError::Shape {
                expected: Shape { rows: 1, columns },
                actual: Shape { rows, columns },
            });
        }
        if let Some(identity) = identity.as_deref_mut() {
            identity.u64(columns as u64);
        }
        convert_grid_elements_with_identity(&grid, argument, context, identity).map(Self)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.0.len() as u64);
        for value in &self.0 {
            value.encode_identity(encoder);
        }
    }
}

impl<'call, T: FromExcel<'call>> FromExcel<'call> for Column<T> {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if columns != 1 {
            return Err(XllError::Shape {
                expected: Shape { rows, columns: 1 },
                actual: Shape { rows, columns },
            });
        }
        if let Some(identity) = identity.as_deref_mut() {
            identity.u64(rows as u64);
        }
        convert_grid_elements_with_identity(&grid, argument, context, identity).map(Self)
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.0.len() as u64);
        for value in &self.0 {
            value.encode_identity(encoder);
        }
    }
}

impl<'call> FromExcel<'call> for ExcelCellValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_NUM | XLTYPE_INT => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelCellValueKind::Number as u8);
                }
                <f64 as FromExcel>::from_excel_with_identity(value, argument, context, identity)
                    .map(Self::Number)
            }
            XLTYPE_BOOL => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelCellValueKind::Boolean as u8);
                }
                <bool as FromExcel>::from_excel_with_identity(value, argument, context, identity)
                    .map(Self::Boolean)
            }
            XLTYPE_STR => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelCellValueKind::String as u8);
                }
                <String as FromExcel>::from_excel_with_identity(value, argument, context, identity)
                    .map(Self::String)
            }
            XLTYPE_ERR => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelCellValueKind::Error as u8);
                }
                <ExcelErrorValue as FromExcel>::from_excel_with_identity(
                    value, argument, context, identity,
                )
                .map(|ExcelErrorValue(error)| Self::Error(error))
            }
            XLTYPE_NIL => {
                if let Some(identity) = identity {
                    identity.tag(ExcelCellValueKind::Blank as u8);
                }
                Ok(Self::Blank)
            }
            _ => Err(value.wrong_type(argument, "worksheet value")),
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Number(value) => {
                encoder.tag(ExcelCellValueKind::Number as u8);
                FromExcel::encode_identity(value, encoder);
            }
            Self::Boolean(value) => {
                encoder.tag(ExcelCellValueKind::Boolean as u8);
                FromExcel::encode_identity(value, encoder);
            }
            Self::String(value) => {
                encoder.tag(ExcelCellValueKind::String as u8);
                FromExcel::encode_identity(value, encoder);
            }
            Self::Error(value) => {
                encoder.tag(ExcelCellValueKind::Error as u8);
                encoder.i64(i64::from(value.code()));
            }
            Self::Blank => encoder.tag(ExcelCellValueKind::Blank as u8),
        }
    }
}

impl<'call> FromExcel<'call> for ExcelValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        <Self as FromExcel>::from_excel_with_identity(
            value,
            argument,
            &CallContext::without_runtime(),
            None,
        )
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        mut identity: Option<&mut InputIdentityEncoder>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING => {
                if let Some(identity) = identity {
                    identity.tag(ExcelValueKind::Missing as u8);
                }
                Ok(Self::Missing)
            }
            XLTYPE_MULTI => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelValueKind::Array as u8);
                }
                <Matrix<ExcelCellValue> as FromExcel>::from_excel_with_identity(
                    value, argument, context, identity,
                )
                .map(Self::Array)
            }
            _ => {
                if let Some(identity) = identity.as_deref_mut() {
                    identity.tag(ExcelValueKind::Scalar as u8);
                }
                <ExcelCellValue as FromExcel>::from_excel_with_identity(
                    value, argument, context, identity,
                )
                .map(Self::Scalar)
            }
        }
    }

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Scalar(value) => {
                encoder.tag(ExcelValueKind::Scalar as u8);
                FromExcel::encode_identity(value, encoder);
            }
            Self::Missing => encoder.tag(ExcelValueKind::Missing as u8),
            Self::Array(value) => {
                encoder.tag(ExcelValueKind::Array as u8);
                FromExcel::encode_identity(value, encoder);
            }
        }
    }
}

impl IntoExcel for ExcelCellOutput {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        if matches!(self, Self::Number(value) if !value.is_finite()) {
            return Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            });
        }
        Ok(self)
    }
}

impl IntoExcel for f64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        if self.is_finite() {
            Ok(ExcelCellOutput::Number(self))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            })
        }
    }
}

impl IntoExcel for bool {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Boolean(self))
    }
}

impl IntoExcel for i32 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Number(self as f64))
    }
}

impl IntoExcel for i64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            Ok(ExcelCellOutput::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoExcel for ExcelSerialDate {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        IntoExcel::into_excel(self.serial)
    }
}

impl IntoExcel for String {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::String(self))
    }
}

impl IntoExcel for &str {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::String(self.to_owned()))
    }
}

impl IntoExcel for ExcelErrorValue {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Error(self.0))
    }
}

impl ExcelReturn for ExcelOutput {
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        Ok(self)
    }
}

impl MainThreadReturn for ExcelOutput {}
impl ThreadSafeReturn for ExcelOutput {}
impl MacroSheetReturn for ExcelOutput {}
impl AsyncReturn for ExcelOutput {}
impl VolatileReturn for ExcelOutput {}

impl<T: IntoExcel> ExcelReturn for Matrix<T> {
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        let mut builder = XlArrayBuilder::new(self.rows, self.columns)?;
        for value in self.data {
            builder.push(value)?;
        }
        builder.finish().map(ExcelOutput::Array)
    }
}

impl<T: IntoExcel> MainThreadReturn for Matrix<T> {}
impl<T: IntoExcel> ThreadSafeReturn for Matrix<T> {}
impl<T: IntoExcel> MacroSheetReturn for Matrix<T> {}
impl<T: IntoExcel> AsyncReturn for Matrix<T> {}
impl<T: IntoExcel> VolatileReturn for Matrix<T> {}

impl<T: IntoExcel> ExcelReturn for Row<T> {
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        let mut builder = XlArrayBuilder::new(1, self.0.len())?;
        for value in self.0 {
            builder.push(value)?;
        }
        builder.finish().map(ExcelOutput::Array)
    }
}

impl<T: IntoExcel> MainThreadReturn for Row<T> {}
impl<T: IntoExcel> ThreadSafeReturn for Row<T> {}
impl<T: IntoExcel> MacroSheetReturn for Row<T> {}
impl<T: IntoExcel> AsyncReturn for Row<T> {}
impl<T: IntoExcel> VolatileReturn for Row<T> {}

impl<T: IntoExcel> ExcelReturn for Column<T> {
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        let mut builder = XlArrayBuilder::new(self.0.len(), 1)?;
        for value in self.0 {
            builder.push(value)?;
        }
        builder.finish().map(ExcelOutput::Array)
    }
}

impl<T: IntoExcel> MainThreadReturn for Column<T> {}
impl<T: IntoExcel> ThreadSafeReturn for Column<T> {}
impl<T: IntoExcel> MacroSheetReturn for Column<T> {}
impl<T: IntoExcel> AsyncReturn for Column<T> {}
impl<T: IntoExcel> VolatileReturn for Column<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use static_assertions::assert_impl_all;
    use xlfn_sys::{XLBIT_XL_FREE, XLOPER12Value};

    assert_impl_all!(ExcelValue: std::panic::UnwindSafe, std::panic::RefUnwindSafe);
    assert_impl_all!(
        XlArrayBuilder: std::panic::UnwindSafe, std::panic::RefUnwindSafe
    );

    fn convert<T>(raw: &mut XLOPER12) -> XllResult<T>
    where
        T: for<'call> ExcelParameter<'call>,
    {
        // SAFETY: raw is live for this conversion.
        with_excel_call_scope(|scope| unsafe { argument_from_raw(scope, "arg", raw) })
    }

    fn convert_with_identity<T>(
        raw: &mut XLOPER12,
    ) -> XllResult<(T, crate::input_identity::InputFingerprint)>
    where
        T: for<'call> ExcelParameter<'call>,
    {
        with_excel_call_scope(|_scope| {
            let mut builder = crate::input_identity::InputFingerprintBuilder::new();
            // SAFETY: raw is live for this conversion.
            let value = unsafe {
                let value_ref = XlValueRef::from_raw(raw)?;
                let mut converted = None;
                builder.with_argument("arg", |encoder| {
                    converted = Some(T::from_excel_with_identity(
                        value_ref,
                        "arg",
                        &CallContext::without_runtime(),
                        Some(encoder),
                    )?);
                    Ok(())
                })?;
                converted.unwrap()
            };
            let fingerprint = builder.finish();
            Ok((value, fingerprint))
        })
    }

    fn identity<'call, T: ExcelParameter<'call>>(
        value: &T,
    ) -> crate::input_identity::InputFingerprint {
        let mut builder = crate::input_identity::InputFingerprintBuilder::new();
        builder
            .with_argument("arg", |encoder| {
                value.encode_identity(encoder);
                Ok(())
            })
            .unwrap();
        builder.finish()
    }

    #[test]
    fn integer_conversion_checks_fraction_and_range() {
        let mut fractional = XLOPER12::number(1.5);
        assert!(matches!(
            convert::<i32>(&mut fractional),
            Err(XllError::Input {
                reason: InputError::NotInteger,
                ..
            })
        ));

        let mut huge = XLOPER12::number(i32::MAX as f64 + 1.0);
        assert!(matches!(
            convert::<i32>(&mut huge),
            Err(XllError::Input {
                reason: InputError::NumericOverflow,
                ..
            })
        ));
    }

    proptest! {
        #[test]
        fn integer_values_round_trip_through_excel_storage(value in any::<i32>()) {
            let mut raw = XLOPER12::integer(value);
            prop_assert_eq!(convert::<i32>(&mut raw).unwrap(), value);
        }
    }

    #[test]
    fn missing_and_blank_remain_distinct() {
        let mut missing = XLOPER12::missing();
        let mut blank = XLOPER12::nil();
        assert_eq!(
            convert::<OptionalExcelValue<f64>>(&mut missing).unwrap(),
            OptionalExcelValue::Missing
        );
        assert_eq!(
            convert::<OptionalExcelValue<f64>>(&mut blank).unwrap(),
            OptionalExcelValue::Blank
        );
    }

    #[test]
    fn return_trait_resolves_result_aliases_without_name_matching() {
        type AliasedReturn = Result<f64, XllError>;
        let mut context = crate::ReturnContext::new();
        let value =
            <AliasedReturn as ExcelReturn>::into_excel(Ok::<_, XllError>(4.5), &mut context)
                .unwrap();
        assert!(matches!(
            value,
            ExcelOutput::Scalar(ExcelCellOutput::Number(number)) if number == 4.5
        ));
    }

    #[test]
    fn result_and_collection_returns_forward_all_standard_modes() {
        fn assert_modes<T>()
        where
            T: MainThreadReturn
                + ThreadSafeReturn
                + MacroSheetReturn
                + AsyncReturn
                + VolatileReturn,
        {
        }

        assert_modes::<f64>();
        assert_modes::<Result<f64, XllError>>();
        assert_modes::<Matrix<f64>>();
        assert_modes::<crate::RtdValue>();
    }

    #[test]
    fn serial_date_keeps_workbook_system_unresolved() {
        let mut raw = XLOPER12::number(60.25);
        let date: ExcelSerialDate = convert(&mut raw).unwrap();
        assert_eq!(date.serial(), 60.25);
        assert_eq!(date.date_system(), ExcelDateSystem::Workbook);
        let date = date.with_date_system(ExcelDateSystem::Windows1900);
        assert!(date.is_fictitious_1900_leap_day());
        assert_eq!(date.fractional_day(), 0.25);
    }

    #[test]
    fn matrix_column_is_checked() {
        let matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(
            matrix.column(1).unwrap().copied().collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert!(matrix.column(2).is_none());
    }

    #[test]
    fn matrix_index_rejects_each_out_of_bounds_dimension_before_flattening() {
        let matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(matrix[(1, 1)], 4);
        assert!(
            std::panic::catch_unwind(|| matrix[(usize::MAX, 2)]).is_err(),
            "overflowing coordinates must not wrap onto a valid element"
        );
        assert!(std::panic::catch_unwind(|| matrix[(0, 2)]).is_err());
        assert!(std::panic::catch_unwind(|| matrix[(2, 0)]).is_err());
    }

    #[test]
    fn strict_utf16_rejects_unpaired_surrogate() {
        let mut text = vec![1_u16, 0xd800];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR | XLBIT_XL_FREE,
        };
        assert!(matches!(
            convert::<String>(&mut raw),
            Err(XllError::Input {
                reason: InputError::InvalidUtf16,
                ..
            })
        ));
    }

    #[test]
    fn borrowed_string_reports_the_named_argument_when_decoding_is_deferred() {
        let mut text = vec![1_u16, 0xd800];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR | XLBIT_XL_FREE,
        };

        with_excel_call_scope(|_| {
            // SAFETY: raw and its UTF-16 payload remain live for this scope.
            let value = unsafe { XlValueRef::from_raw(&mut raw) }.unwrap();
            let string = value.as_str_with_argument("currency").unwrap();
            assert!(matches!(
                string.to_string(),
                Err(XllError::Input {
                    argument: "currency",
                    reason: InputError::InvalidUtf16,
                })
            ));
        });
    }

    #[test]
    fn matrix_is_read_in_row_major_order() {
        let mut elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
            XLOPER12::number(4.0),
        ];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 2,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let matrix = convert::<Matrix<f64>>(&mut raw).unwrap();
        assert_eq!(matrix.rows(), 2);
        assert_eq!(matrix.columns(), 2);
        assert_eq!(matrix.as_slice(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn scalar_values_lift_to_one_by_one_collections() {
        let mut number = XLOPER12::number(7.0);
        let matrix = convert::<Matrix<f64>>(&mut number).unwrap();
        assert_eq!((matrix.rows(), matrix.columns()), (1, 1));
        assert_eq!(matrix.as_slice(), &[7.0]);

        let mut number = XLOPER12::number(7.0);
        assert_eq!(convert::<Row<f64>>(&mut number).unwrap().as_slice(), &[7.0]);
        let mut number = XLOPER12::number(7.0);
        assert_eq!(
            convert::<Column<f64>>(&mut number).unwrap().as_slice(),
            &[7.0]
        );
        let mut number = XLOPER12::number(7.0);
        assert_eq!(convert::<Vec<f64>>(&mut number).unwrap(), vec![7.0]);
    }

    #[test]
    fn bounded_varargs_enforce_the_type_level_limit() {
        let mut elements = vec![XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_eq!(
            convert::<BoundedVarArgs<f64, 2>>(&mut raw)
                .unwrap()
                .as_slice(),
            &[1.0, 2.0]
        );
        assert!(matches!(
            convert::<BoundedVarArgs<f64, 1>>(&mut raw),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1,
                    actual: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn bounded_varargs_rejects_oversized_input_before_converting_elements() {
        struct PanicOnConvert;
        impl<'call> FromExcel<'call> for PanicOnConvert {
            fn from_excel(_value: XlValueRef<'call>, _argument: &'static str) -> XllResult<Self> {
                panic!("element conversion should not occur for oversized inputs");
            }

            fn encode_identity(&self, _: &mut InputIdentityEncoder) {}
        }

        let mut elements = vec![XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        let result = convert::<BoundedVarArgs<PanicOnConvert, 1>>(&mut raw);
        assert!(matches!(
            result,
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1,
                    actual: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn blank_and_error_elements_keep_existing_conversion_rules() {
        let mut elements = vec![XLOPER12::nil(), XLOPER12::error(xlfn_sys::XLERR_NA)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let values = convert::<Matrix<ExcelCellValue>>(&mut raw).unwrap();
        assert_eq!(values.as_slice()[0], ExcelCellValue::Blank);
        assert_eq!(
            values.as_slice()[1],
            ExcelCellValue::Error(ExcelError::NotAvailable)
        );
    }

    #[test]
    fn dynamic_values_separate_missing_from_blank_and_canonicalize_integers() {
        let mut missing = XLOPER12::missing();
        assert_eq!(
            convert::<ExcelValue>(&mut missing).unwrap(),
            ExcelValue::Missing
        );

        let mut blank = XLOPER12::nil();
        assert_eq!(
            convert::<ExcelValue>(&mut blank).unwrap(),
            ExcelValue::Scalar(ExcelCellValue::Blank)
        );

        let mut integer = XLOPER12::integer(7);
        assert_eq!(
            convert::<ExcelValue>(&mut integer).unwrap(),
            ExcelValue::Scalar(ExcelCellValue::Number(7.0))
        );

        let mut cells = [XLOPER12::nil(), XLOPER12::integer(8)];
        let mut array = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_eq!(
            convert::<ExcelValue>(&mut array).unwrap(),
            ExcelValue::Array(
                Matrix::new(
                    1,
                    2,
                    vec![ExcelCellValue::Blank, ExcelCellValue::Number(8.0)],
                )
                .unwrap(),
            )
        );
    }

    #[test]
    fn non_finite_values_are_rejected_both_directions() {
        let mut raw = XLOPER12::number(f64::NAN);
        assert!(convert::<f64>(&mut raw).is_err());
        assert!(IntoExcel::into_excel(f64::INFINITY).is_err());
    }

    #[test]
    fn typed_arguments_propagate_excel_error_values() {
        let mut raw = XLOPER12::error(xlfn_sys::XLERR_NA);
        let error = convert::<f64>(&mut raw).unwrap_err();
        assert_eq!(error.excel_error(), ExcelError::NotAvailable);
    }

    #[test]
    fn malformed_xltype_flags_are_rejected() {
        let mut raw = XLOPER12::number(1.0);
        raw.xltype |= 0x2000;
        assert!(matches!(
            convert::<f64>(&mut raw),
            Err(XllError::Input {
                reason: InputError::Malformed("unknown xltype flag"),
                ..
            })
        ));
    }

    #[test]
    fn custom_conversion_can_return_owned_data() {
        #[derive(Debug, PartialEq)]
        struct FiniteNumber(f64);

        impl<'call> FromExcel<'call> for FiniteNumber {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                <f64 as FromExcel>::from_excel(value, argument).map(Self)
            }

            fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
                FromExcel::encode_identity(&self.0, encoder);
            }
        }

        let mut raw = XLOPER12::number(42.0);
        assert_eq!(
            convert::<FiniteNumber>(&mut raw).unwrap(),
            FiniteNumber(42.0)
        );
    }

    #[test]
    fn borrowed_array_reads_cells_without_materializing_them() {
        let mut elements = [
            XLOPER12::number(1.5),
            XLOPER12::integer(2),
            XLOPER12::boolean(true),
            XLOPER12::nil(),
        ];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 2,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: raw and its four cells remain live inside this scope.
            let view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "values", &mut raw) }.unwrap();
            assert_eq!(view.shape(), (2, 2));
            assert_eq!(view.get(0, 0).unwrap().as_f64().unwrap(), 1.5);
            assert_eq!(view.get(0, 1).unwrap().as_f64().unwrap(), 2.0);
            assert!(view.get(1, 0).unwrap().as_bool().unwrap());
            assert!(view.get(1, 1).unwrap().is_blank());
        });
    }

    #[test]
    fn raw_array_views_preserve_raw_numeric_bits() {
        let mut negative_cell = [XLOPER12::number(-0.0)];
        let mut positive_cell = [XLOPER12::number(0.0)];
        let mut negative = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: negative_cell.as_mut_ptr(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut positive = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: positive_cell.as_mut_ptr(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: both arrays and their cells remain live for this scope.
            let negative_view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "negative", &mut negative) }.unwrap();
            // SAFETY: both arrays and their cells remain live for this scope.
            let positive_view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "positive", &mut positive) }.unwrap();
            assert_ne!(identity(&negative_view), identity(&positive_view));
        });
    }

    #[test]
    fn borrowed_array_rejects_a_misaligned_cell_buffer() {
        let mut storage = [XLOPER12::nil(), XLOPER12::nil()];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    // Deliberately misaligned; validation must reject it before reading.
                    values: storage.as_mut_ptr().cast::<u8>().wrapping_add(1).cast(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        with_excel_call_scope(|scope| {
            // SAFETY: the root is live; the malformed nested pointer is tested for rejection.
            let result = unsafe { argument_from_raw::<XlArrayRef<'_>>(scope, "values", &mut raw) };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    reason: InputError::Malformed("misaligned array pointer"),
                    ..
                })
            ));
        });
    }

    #[test]
    fn array_builder_encodes_directly_into_its_finished_cell_buffer() {
        let mut builder = XlArrayBuilder::new(2, 2).unwrap();
        for value in [1.0, 2.0, 3.0, 4.0] {
            builder.push_f64(value).unwrap();
        }
        let encoded = builder.finish().unwrap();
        assert_eq!((encoded.rows, encoded.columns), (2, 2));
        assert_eq!(encoded.cells.len(), 4);
        for (cell, expected) in encoded.cells.iter().zip([1.0, 2.0, 3.0, 4.0]) {
            assert_eq!(cell.base_type(), XLTYPE_NUM);
            // SAFETY: XLTYPE_NUM selects the number member.
            assert_eq!(unsafe { cell.value.number }, expected);
        }
    }

    #[test]
    fn matrix_dimensions_must_fit_a_non_empty_worksheet_shape() {
        assert!(Matrix::<f64>::new(0, 1, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(1, 0, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(EXCEL_MAX_ROWS + 1, 1, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(1, EXCEL_MAX_COLUMNS + 1, Vec::new()).is_err());
    }

    #[test]
    fn oversized_excel_dimensions_are_rejected_before_element_access() {
        for (rows, columns, limit, actual) in [
            (
                i32::try_from(EXCEL_MAX_ROWS + 1).unwrap(),
                1,
                EXCEL_MAX_ROWS,
                EXCEL_MAX_ROWS + 1,
            ),
            (
                1,
                i32::try_from(EXCEL_MAX_COLUMNS + 1).unwrap(),
                EXCEL_MAX_COLUMNS,
                EXCEL_MAX_COLUMNS + 1,
            ),
        ] {
            let mut raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        values: std::ptr::null_mut(),
                        rows,
                        columns,
                    },
                },
                xltype: XLTYPE_MULTI,
            };

            assert!(matches!(
                convert::<Matrix<f64>>(&mut raw),
                Err(XllError::Input {
                    reason: InputError::TooLarge {
                        limit: error_limit,
                        actual: error_actual,
                    },
                    ..
                }) if error_limit == limit && error_actual == actual
            ));
        }
    }

    #[test]
    fn matrix_number_return_uses_encoded_array_output() {
        let matrix = Matrix::new(1, 2, vec![1.0, 2.0]).unwrap();
        let value =
            <Matrix<f64> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new()).unwrap();
        assert!(matches!(value, ExcelOutput::Array(_)));
    }

    #[test]
    fn element_conversion_is_called_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedCell<'a> {
            conversions: &'a AtomicUsize,
            value: f64,
        }

        impl IntoExcel for CountedCell<'_> {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                self.conversions.fetch_add(1, Ordering::Relaxed);
                IntoExcel::into_excel(self.value)
            }
        }

        let conversions = AtomicUsize::new(0);
        let data: Vec<_> = (0..1000)
            .map(|i| CountedCell {
                conversions: &conversions,
                value: i as f64,
            })
            .collect();
        let matrix = Matrix::new(10, 100, data).unwrap();
        let _value =
            <Matrix<CountedCell<'_>> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new())
                .unwrap();
        assert_eq!(conversions.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn partial_failure_during_matrix_conversion_cleans_up_safely() {
        let data = vec![1.0, 2.0, f64::NAN, 4.0];
        let matrix = Matrix::new(2, 2, data).unwrap();
        let result = <Matrix<f64> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new());
        assert!(result.is_err());
    }

    #[test]
    fn f64_semantic_identity_canonicalizes_integer_representation() {
        let mut int_raw = XLOPER12::integer(1);
        let mut num_raw = XLOPER12::number(1.0);
        let (int_val, int_id) = convert_with_identity::<f64>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<f64>(&mut num_raw).unwrap();
        assert_eq!(int_val, 1.0);
        assert_eq!(num_val, 1.0);
        assert_eq!(int_id, num_id);

        let mut pos_zero = XLOPER12::number(0.0);
        let mut neg_zero = XLOPER12::number(-0.0);
        let (_, pos_id) = convert_with_identity::<f64>(&mut pos_zero).unwrap();
        let (_, neg_id) = convert_with_identity::<f64>(&mut neg_zero).unwrap();
        assert_ne!(pos_id, neg_id);
    }

    #[test]
    fn i32_semantic_identity_canonicalizes_integer_and_number() {
        let mut int_raw = XLOPER12::integer(42);
        let mut num_raw = XLOPER12::number(42.0);
        let (int_val, int_id) = convert_with_identity::<i32>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<i32>(&mut num_raw).unwrap();
        assert_eq!(int_val, 42);
        assert_eq!(num_val, 42);
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn vec_semantic_identity_ignores_1d_orientation() {
        let mut row_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut col_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut row_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: row_elements.as_mut_ptr(),
                    rows: 1,
                    columns: 3,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut col_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: col_elements.as_mut_ptr(),
                    rows: 3,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let (row_vec, row_id) = convert_with_identity::<Vec<f64>>(&mut row_raw).unwrap();
        let (col_vec, col_id) = convert_with_identity::<Vec<f64>>(&mut col_raw).unwrap();
        assert_eq!(row_vec, vec![1.0, 2.0, 3.0]);
        assert_eq!(col_vec, vec![1.0, 2.0, 3.0]);
        assert_eq!(row_id, col_id);
    }

    #[test]
    fn matrix_semantic_identity_observes_orientation() {
        let mut row_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut col_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut row_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: row_elements.as_mut_ptr(),
                    rows: 1,
                    columns: 3,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut col_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: col_elements.as_mut_ptr(),
                    rows: 3,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let (row_mat, row_id) = convert_with_identity::<Matrix<f64>>(&mut row_raw).unwrap();
        let (col_mat, col_id) = convert_with_identity::<Matrix<f64>>(&mut col_raw).unwrap();
        assert_eq!((row_mat.rows(), row_mat.columns()), (1, 3));
        assert_eq!((col_mat.rows(), col_mat.columns()), (3, 1));
        assert_ne!(row_id, col_id);
    }

    #[test]
    fn excel_cell_value_canonicalizes_numbers_into_same_identity() {
        let mut int_raw = XLOPER12::integer(10);
        let mut num_raw = XLOPER12::number(10.0);
        let (int_cell, int_id) = convert_with_identity::<ExcelCellValue>(&mut int_raw).unwrap();
        let (num_cell, num_id) = convert_with_identity::<ExcelCellValue>(&mut num_raw).unwrap();
        assert_eq!(int_cell, ExcelCellValue::Number(10.0));
        assert_eq!(num_cell, ExcelCellValue::Number(10.0));
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn excel_value_semantic_identity_canonicalizes_scalars_and_preserves_array_shape() {
        let mut int_raw = XLOPER12::integer(10);
        let mut num_raw = XLOPER12::number(10.0);
        let (int_val, int_id) = convert_with_identity::<ExcelValue>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<ExcelValue>(&mut num_raw).unwrap();
        assert_eq!(int_val, ExcelValue::Scalar(ExcelCellValue::Number(10.0)));
        assert_eq!(num_val, ExcelValue::Scalar(ExcelCellValue::Number(10.0)));
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn option_and_optional_excel_value_missing_and_blank_identities() {
        let mut missing_raw = XLOPER12::missing();
        let mut blank_raw = XLOPER12::nil();
        let (opt_m, id_m) = convert_with_identity::<Option<f64>>(&mut missing_raw).unwrap();
        let (opt_b, id_b) = convert_with_identity::<Option<f64>>(&mut blank_raw).unwrap();
        assert_eq!(opt_m, None);
        assert_eq!(opt_b, None);
        assert_eq!(id_m, id_b);

        let mut missing_raw2 = XLOPER12::missing();
        let mut blank_raw2 = XLOPER12::nil();
        let (opt_val_m, id_val_m) =
            convert_with_identity::<OptionalExcelValue<f64>>(&mut missing_raw2).unwrap();
        let (opt_val_b, id_val_b) =
            convert_with_identity::<OptionalExcelValue<f64>>(&mut blank_raw2).unwrap();
        assert_eq!(opt_val_m, OptionalExcelValue::Missing);
        assert_eq!(opt_val_b, OptionalExcelValue::Blank);
        assert_ne!(id_val_m, id_val_b);
    }

    #[derive(Debug, PartialEq)]
    struct SemanticHandleTestObj {
        data: i32,
    }
    impl crate::handle::ExcelHandleObject for SemanticHandleTestObj {}

    #[test]
    fn handle_semantic_identity_matches_across_distinct_alias_tokens() {
        use crate::handle::{FormulaCaller, FormulaRevisionKey, HandleTopicKey};

        let runtime = Box::leak(Box::new(crate::Runtime::<()>::new()));
        runtime.arm_test_generation();
        let handle_rt = runtime.handles().unwrap();

        let topic_a = HandleTopicKey::Formula(FormulaRevisionKey::new(
            FormulaCaller {
                sheet_id: 1,
                row: 1,
                column: 1,
            },
            "FUNC.A",
            crate::input_identity::InputFingerprint::from_bytes([1; 32]),
        ));
        let topic_b = HandleTopicKey::Formula(FormulaRevisionKey::new(
            FormulaCaller {
                sheet_id: 1,
                row: 2,
                column: 2,
            },
            "FUNC.B",
            crate::input_identity::InputFingerprint::from_bytes([2; 32]),
        ));

        let (token_a, _) = handle_rt
            .prepare::<SemanticHandleTestObj, _>(topic_a, || Ok(SemanticHandleTestObj { data: 99 }))
            .unwrap();

        let object = crate::with_excel_call_scope(|scope| {
            let resolved: crate::Handle<'_, SemanticHandleTestObj> =
                handle_rt.lookup(scope, &token_a).unwrap();
            resolved.alias().into_locator()
        });

        let (token_b, _) = handle_rt
            .prepare_observed_alias::<SemanticHandleTestObj, _>(topic_b, object, |_, _| Ok(()))
            .unwrap();

        assert_ne!(token_a, token_b);

        let mut str_bytes_a: Vec<u16> = std::iter::once(token_a.len() as u16)
            .chain(token_a.encode_utf16())
            .collect();
        let mut raw_a = XLOPER12 {
            value: XLOPER12Value {
                string: str_bytes_a.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        let mut str_bytes_b: Vec<u16> = std::iter::once(token_b.len() as u16)
            .chain(token_b.encode_utf16())
            .collect();
        let mut raw_b = XLOPER12 {
            value: XLOPER12Value {
                string: str_bytes_b.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        let (handle_data_a, id_a, object_id_a) = crate::with_excel_call_scope(|scope| {
            let mut arguments = ArgumentContext {
                call: CallContext::new(runtime, scope),
                inputs: Some(crate::input_identity::InputFingerprintBuilder::new()),
            };
            // SAFETY: raw_a is live for this conversion.
            let handle = unsafe {
                argument_from_raw_with_arguments::<crate::Handle<'_, SemanticHandleTestObj>>(
                    &mut arguments,
                    "arg",
                    &mut raw_a,
                )
            }
            .unwrap();
            let id = arguments.inputs.unwrap().finish();
            (handle.data, id, handle.object.id)
        });

        let (handle_data_b, id_b, object_id_b) = crate::with_excel_call_scope(|scope| {
            let mut arguments = ArgumentContext {
                call: CallContext::new(runtime, scope),
                inputs: Some(crate::input_identity::InputFingerprintBuilder::new()),
            };
            // SAFETY: raw_b is live for this conversion.
            let handle = unsafe {
                argument_from_raw_with_arguments::<crate::Handle<'_, SemanticHandleTestObj>>(
                    &mut arguments,
                    "arg",
                    &mut raw_b,
                )
            }
            .unwrap();
            let id = arguments.inputs.unwrap().finish();
            (handle.data, id, handle.object.id)
        });

        assert_eq!(handle_data_a, 99);
        assert_eq!(handle_data_b, 99);
        assert_eq!(object_id_a, object_id_b);
        assert_eq!(id_a, id_b);
    }
}
