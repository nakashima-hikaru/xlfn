use crate::host_callback::HostCallbackSession;
use crate::return_storage::ReturnStorage;
use crate::{
    DomainErrorCode, ExcelError, InputError, IntoXllError, ReturnContext, Shape, XllError,
    XllResult,
};
use std::marker::PhantomData;
use std::ops::Index;
use std::rc::Rc;
use std::slice;
use xlfn_sys::{
    XLBIT_DLL_FREE, XLBIT_XL_FREE, XLOPER12, XLOPER12Array, XLTYPE_BOOL, XLTYPE_ERR, XLTYPE_INT,
    XLTYPE_MASK, XLTYPE_MISSING, XLTYPE_MULTI, XLTYPE_NIL, XLTYPE_NUM, XLTYPE_STR,
};

const MAX_UTF16_UNITS: usize = 32_767;
const EXCEL_MAX_ROWS: usize = 1_048_576;
const EXCEL_MAX_COLUMNS: usize = 16_384;
#[cfg(target_pointer_width = "32")]
const MAX_ARRAY_ELEMENTS: usize = 1_000_000;
#[cfg(not(target_pointer_width = "32"))]
const MAX_ARRAY_ELEMENTS: usize = 4_000_000;
#[cfg(target_pointer_width = "32")]
const MAX_ARRAY_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_pointer_width = "32"))]
const MAX_ARRAY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct XlValueRef<'call> {
    raw: &'call XLOPER12,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

enum GridView<'call> {
    Scalar(XlValueRef<'call>),
    Multi {
        rows: usize,
        columns: usize,
        values: *mut XLOPER12,
        _lifetime: PhantomData<&'call XLOPER12>,
    },
}

impl<'call> GridView<'call> {
    fn from_value(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.base_type() == XLTYPE_MULTI {
            let array = value.array(argument)?;
            Ok(Self::Multi {
                rows: array.rows as usize,
                columns: array.columns as usize,
                values: array.values,
                _lifetime: PhantomData,
            })
        } else {
            Ok(Self::Scalar(value))
        }
    }

    const fn shape(&self) -> (usize, usize) {
        match self {
            Self::Scalar(_) => (1, 1),
            Self::Multi { rows, columns, .. } => (*rows, *columns),
        }
    }

    fn element(&self, index: usize) -> XllResult<XlValueRef<'call>> {
        match self {
            Self::Scalar(value) if index == 0 => Ok(*value),
            Self::Scalar(_) => Err(XllError::Internal {
                diagnostic_id: 0x4752_4944_494E_4458,
            }),
            Self::Multi { values, .. } => {
                // SAFETY: `array` validation established the contiguous range,
                // and callers only request indices within the validated shape.
                unsafe { XlValueRef::from_raw(values.add(index)) }
            }
        }
    }
}

impl<'call> XlValueRef<'call> {
    /// Creates a call-scoped view over an argument supplied by Excel.
    ///
    /// # Safety
    ///
    /// `raw` must be non-null, aligned, and point to a live XLOPER12 for
    /// `'call`. Any nested pointers selected by `xltype` must satisfy the
    /// corresponding Excel SDK contract.
    pub unsafe fn from_raw(raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: The caller guarantees a live, aligned XLOPER12 for 'call.
        let raw = unsafe { raw.as_ref() }
            .ok_or_else(|| XllError::input("<raw>", InputError::NullPointer))?;
        if raw.xltype & !(XLTYPE_MASK | XLBIT_XL_FREE | XLBIT_DLL_FREE) != 0 {
            return Err(XllError::input(
                "<raw>",
                InputError::Malformed("unknown xltype flag"),
            ));
        }
        Ok(Self {
            raw,
            _not_send_or_sync: PhantomData,
        })
    }

    #[must_use]
    #[inline]
    pub const fn base_type(&self) -> u32 {
        self.raw.base_type()
    }

    #[must_use]
    #[inline]
    pub const fn raw(&self) -> &'call XLOPER12 {
        self.raw
    }

    /// Returns this value as a finite Excel number without allocating.
    #[inline]
    pub fn as_f64(self) -> XllResult<f64> {
        f64::from_excel(self, "<array cell>", &CallContext::without_runtime())
    }

    /// Returns this value as an Excel boolean without allocating.
    #[inline]
    pub fn as_bool(self) -> XllResult<bool> {
        bool::from_excel(self, "<array cell>", &CallContext::without_runtime())
    }

    /// Borrows the UTF-16 payload of an Excel string without decoding it.
    #[inline]
    pub fn as_str(self) -> XllResult<XlStrRef<'call>> {
        self.as_str_with_argument("<array cell>")
    }

    /// Borrows an Excel string while preserving the caller's argument name in
    /// conversion errors. This is used by allocation-free generated enum
    /// conversions.
    #[inline]
    pub fn as_str_with_argument(self, argument: &'static str) -> XllResult<XlStrRef<'call>> {
        Ok(XlStrRef {
            utf16: self.utf16(argument)?,
            argument,
        })
    }

    #[must_use]
    #[inline]
    pub const fn is_blank(self) -> bool {
        self.base_type() == XLTYPE_NIL
    }

    fn wrong_type(&self, argument: &'static str, expected: &'static str) -> XllError {
        if self.base_type() == XLTYPE_ERR {
            // SAFETY: XLTYPE_ERR selects the error union member.
            let code = unsafe { self.raw.value.error };
            return ExcelError::from_code(code).map_or_else(
                || XllError::input(argument, InputError::Malformed("unknown error code")),
                XllError::ExcelValue,
            );
        }
        XllError::input(
            argument,
            InputError::WrongType {
                expected,
                actual: self.base_type(),
            },
        )
    }

    pub(crate) fn utf16(&self, argument: &'static str) -> XllResult<&'call [u16]> {
        if self.base_type() != XLTYPE_STR {
            return Err(self.wrong_type(argument, "string"));
        }
        // SAFETY: XLTYPE_STR selects the string union member.
        let pointer = unsafe { self.raw.value.string };
        if pointer.is_null() {
            return Err(XllError::input(argument, InputError::NullPointer));
        }
        // SAFETY: Excel strings begin with one readable length code unit.
        let length = unsafe { *pointer } as usize;
        if length > MAX_UTF16_UNITS {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: MAX_UTF16_UNITS,
                    actual: length,
                },
            ));
        }
        // SAFETY: The Excel string contract guarantees length following units.
        Ok(unsafe { slice::from_raw_parts(pointer.add(1), length) })
    }

    pub(crate) fn array(&self, argument: &'static str) -> XllResult<XLOPER12Array> {
        if self.base_type() != XLTYPE_MULTI {
            return Err(self.wrong_type(argument, "array"));
        }
        // SAFETY: XLTYPE_MULTI selects the array union member.
        let array = unsafe { self.raw.value.array };
        if array.rows < 0 || array.columns < 0 {
            return Err(XllError::input(
                argument,
                InputError::Malformed("negative array dimension"),
            ));
        }
        let rows = array.rows as usize;
        let columns = array.columns as usize;
        if rows > EXCEL_MAX_ROWS {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: EXCEL_MAX_ROWS,
                    actual: rows,
                },
            ));
        }
        if columns > EXCEL_MAX_COLUMNS {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: EXCEL_MAX_COLUMNS,
                    actual: columns,
                },
            ));
        }
        let elements = rows.checked_mul(columns).ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array dimension overflow"))
        })?;
        if elements > MAX_ARRAY_ELEMENTS {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: MAX_ARRAY_ELEMENTS,
                    actual: elements,
                },
            ));
        }
        let bytes = elements
            .checked_mul(std::mem::size_of::<XLOPER12>())
            .ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
        if bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: bytes,
                },
            ));
        }
        if elements != 0 && array.values.is_null() {
            return Err(XllError::input(argument, InputError::NullPointer));
        }
        if elements != 0
            && !(array.values as usize).is_multiple_of(std::mem::align_of::<XLOPER12>())
        {
            return Err(XllError::input(
                argument,
                InputError::Malformed("misaligned array pointer"),
            ));
        }
        Ok(array)
    }
}

/// A call-scoped, allocation-free view of an Excel UTF-16 string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XlStrRef<'call> {
    utf16: &'call [u16],
    argument: &'static str,
}

impl<'call> XlStrRef<'call> {
    #[must_use]
    #[inline]
    pub const fn as_utf16(self) -> &'call [u16] {
        self.utf16
    }

    pub fn chars(self) -> impl Iterator<Item = Result<char, std::char::DecodeUtf16Error>> + 'call {
        char::decode_utf16(self.utf16.iter().copied())
    }

    pub fn to_string(self) -> XllResult<String> {
        String::from_utf16(self.utf16)
            .map_err(|_| XllError::input(self.argument, InputError::InvalidUtf16))
    }
}

/// A call-scoped view over an Excel `xltypeMulti` value.
///
/// Constructed by `#[excel_function]` wrappers for `XlArrayRef<'_>`
/// parameters. Cells are converted only when the caller asks for a typed
/// value, so iterating or inspecting the shape performs no allocation.
#[derive(Clone, Copy)]
pub struct XlArrayRef<'call> {
    cells: &'call [XLOPER12],
    rows: usize,
    columns: usize,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'call> XlArrayRef<'call> {
    fn from_value(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let array = value.array(argument)?;
        let rows = array.rows as usize;
        let columns = array.columns as usize;
        let len = rows * columns;
        let cells = if len == 0 {
            &[]
        } else {
            // SAFETY: XlValueRef::array validated the non-null pointer,
            // dimensions, byte size, and lifetime of this contiguous range.
            unsafe { slice::from_raw_parts(array.values.cast_const(), len) }
        };
        Ok(Self {
            cells,
            rows,
            columns,
            _not_send_or_sync: PhantomData,
        })
    }

    #[must_use]
    pub const fn rows(self) -> usize {
        self.rows
    }

    #[must_use]
    pub const fn columns(self) -> usize {
        self.columns
    }

    #[must_use]
    pub const fn shape(self) -> (usize, usize) {
        (self.rows, self.columns)
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.cells.is_empty()
    }

    #[must_use]
    pub fn get(self, row: usize, column: usize) -> Option<XlValueRef<'call>> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        let index = row * self.columns + column;
        Some(XlValueRef {
            raw: &self.cells[index],
            _not_send_or_sync: PhantomData,
        })
    }

    pub fn cells(self) -> impl ExactSizeIterator<Item = XlValueRef<'call>> + 'call {
        self.cells.iter().map(|raw| XlValueRef {
            raw,
            _not_send_or_sync: PhantomData,
        })
    }
}

impl<'call> FromExcel<'call> for XlArrayRef<'call> {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
        Self::from_value(value, argument)
    }
}

/// Converts a call-scoped Excel value into owned Rust data.
///
/// The input lifetime is deliberately anonymous: an implementation cannot
/// choose it or store a reference to Excel-owned memory in `Self`.
///
/// ```compile_fail
/// use xlfn_core::{XlValueRef, FromExcel, XllResult};
/// use xlfn_sys::XLOPER12;
///
/// struct Escaped(&'static XLOPER12);
///
/// impl<'call> FromExcel<'call> for Escaped {
///     fn from_excel(
///         value: XlValueRef<'call>,
///         _: &'static str,
///         _: &xlfn_core::CallContext,
///     ) -> XllResult<Self> {
///         Ok(Self(value.raw()))
///     }
/// }
/// ```
pub trait FromExcel<'call>: Sized {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self>;
}

pub trait IntoExcelValue {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue>;
}

pub trait ExcelParameter<'call>: FromExcel<'call> {}

impl<'call, T> ExcelParameter<'call> for T where T: FromExcel<'call> {}

pub(crate) trait HandleRuntimeProvider {
    fn handle_runtime(&self) -> XllResult<std::sync::Arc<crate::handle::HandleRuntime>>;
}

impl<S> HandleRuntimeProvider for crate::Runtime<S> {
    fn handle_runtime(&self) -> XllResult<std::sync::Arc<crate::handle::HandleRuntime>> {
        self.handles()
    }
}

/// Runtime services available while converting one Excel-visible argument.
///
/// The handle runtime is acquired lazily so ordinary scalar conversions do not
/// initialize handle registry state.
pub struct CallContext<'call> {
    runtime: Option<&'call dyn HandleRuntimeProvider>,
    scope: Option<&'call CallScope<'call>>,
}

impl<'call> CallContext<'call> {
    pub(crate) fn new<S>(
        runtime: &'call crate::Runtime<S>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            scope: Some(scope),
        }
    }

    pub(crate) const fn without_runtime() -> Self {
        Self {
            runtime: None,
            scope: None,
        }
    }

    pub(crate) fn resolve_handle<T: crate::handle::ExcelHandleObject>(
        &self,
        token: &str,
    ) -> XllResult<crate::Handle<'call, T>> {
        let scope = self.scope.ok_or(XllError::Internal {
            diagnostic_id: 0x4841_4E44_5343_4F50,
        })?;
        self.runtime
            .ok_or(XllError::Internal {
                diagnostic_id: 0x4841_4E44_4E4F_4354,
            })?
            .handle_runtime()?
            .lookup(scope, token)
    }
}

pub trait ExcelReturn: Sized {
    type Output: IntoExcelValue;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output>;

    #[doc(hidden)]
    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<Self::Output> {
        operation()?.into_excel(context)
    }
}

/// Return values supported by ordinary main-thread worksheet functions.
pub trait MainThreadReturn: ExcelReturn {}

/// Return values supported by Excel multi-threaded recalculation.
pub trait ThreadSafeReturn: ExcelReturn {}

/// Return values supported by macro-sheet functions.
pub trait MacroSheetReturn: ExcelReturn {}

/// Return values supported by native asynchronous functions.
pub trait AsyncReturn: ExcelReturn {}

/// Return values supported by volatile functions.
pub trait VolatileReturn: ExcelReturn {}

impl<T, E> ExcelReturn for Result<T, E>
where
    T: ExcelReturn,
    E: IntoXllError,
{
    type Output = T::Output;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        self.map_err(IntoXllError::into_xll_error)?
            .into_excel(context)
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<Self::Output> {
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
pub fn assert_async_parameter<T>()
where
    T: for<'call> ExcelParameter<'call> + Send + 'static,
{
}

/// A generative lifetime token for one generated Excel call boundary.
#[doc(hidden)]
pub struct CallScope<'call> {
    callbacks: HostCallbackSession,
    lifetime: PhantomData<&'call mut &'call ()>,
}

impl<'call> CallScope<'call> {
    pub(crate) fn callbacks(&'call self) -> &'call HostCallbackSession {
        &self.callbacks
    }
}

/// Runs an operation under a fresh lifetime that cannot escape in its result.
#[doc(hidden)]
pub fn with_excel_call_scope<R>(
    operation: impl for<'scope> FnOnce(&'scope CallScope<'scope>) -> R,
) -> R {
    let scope = CallScope {
        callbacks: HostCallbackSession::new(),
        lifetime: PhantomData,
    };
    operation(&scope)
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
    T: FromExcel<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::from_excel(borrowed, argument, &CallContext::without_runtime())
}

#[doc(hidden)]
pub unsafe fn argument_from_raw_with_context<'call, S, T>(
    _scope: &'call CallScope<'call>,
    runtime: &'call crate::Runtime<S>,
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: FromExcel<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::Input { argument, reason },
        other => other,
    })?;
    T::from_excel(borrowed, argument, &CallContext::new(runtime, _scope))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
pub struct Matrix<T> {
    rows: usize,
    columns: usize,
    data: Vec<T>,
}

impl<T> Matrix<T> {
    pub fn new(rows: usize, columns: usize, data: Vec<T>) -> XllResult<Self> {
        validate_matrix_dimensions(rows, columns, data.len())?;
        Ok(Self {
            rows,
            columns,
            data,
        })
    }
}

fn validate_matrix_dimensions(rows: usize, columns: usize, actual: usize) -> XllResult<()> {
    if rows == 0 || columns == 0 {
        return Err(XllError::input(
            "<matrix>",
            InputError::Malformed("matrix dimensions must be non-zero"),
        ));
    }
    if rows > EXCEL_MAX_ROWS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: EXCEL_MAX_ROWS,
                actual: rows,
            },
        ));
    }
    if columns > EXCEL_MAX_COLUMNS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: EXCEL_MAX_COLUMNS,
                actual: columns,
            },
        ));
    }
    let expected = rows.checked_mul(columns).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })?;
    if expected != actual {
        return Err(XllError::ElementCountMismatch {
            rows,
            columns,
            expected,
            actual,
        });
    }
    if expected > MAX_ARRAY_ELEMENTS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: MAX_ARRAY_ELEMENTS,
                actual: expected,
            },
        ));
    }
    Ok(())
}

impl<T> Matrix<T> {
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.data
    }

    pub fn row(&self, row: usize) -> Option<&[T]> {
        let start = row.checked_mul(self.columns)?;
        let end = start.checked_add(self.columns)?;
        self.data.get(start..end)
    }

    pub fn column(&self, column: usize) -> Option<impl Iterator<Item = &T>> {
        (column < self.columns).then(|| self.data.iter().skip(column).step_by(self.columns))
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }
}

impl<T> Index<(usize, usize)> for Matrix<T> {
    type Output = T;

    fn index(&self, (row, column): (usize, usize)) -> &Self::Output {
        assert!(row < self.rows, "matrix row index out of bounds");
        assert!(column < self.columns, "matrix column index out of bounds");
        let index = row
            .checked_mul(self.columns)
            .and_then(|index| index.checked_add(column))
            .expect("matrix index overflow");
        &self.data[index]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row<T>(Vec<T>);

impl<T> Row<T> {
    pub fn new(data: Vec<T>) -> XllResult<Self> {
        let matrix = Matrix::new(1, data.len(), data)?;
        Ok(Self(matrix.into_vec()))
    }
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Column<T>(Vec<T>);

impl<T> Column<T> {
    pub fn new(data: Vec<T>) -> XllResult<Self> {
        let matrix = Matrix::new(data.len(), 1, data)?;
        Ok(Self(matrix.into_vec()))
    }
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedVarArgs<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVarArgs<T, MAX> {
    pub fn new(values: Vec<T>) -> XllResult<Self> {
        if MAX == 0 {
            return Err(XllError::input(
                "<varargs>",
                InputError::Malformed("bounded varargs maximum must be non-zero"),
            ));
        }
        if values.len() > MAX {
            return Err(XllError::input(
                "<varargs>",
                InputError::TooLarge {
                    limit: MAX,
                    actual: values.len(),
                },
            ));
        }
        Ok(Self(values))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExcelDateSystem {
    /// The workbook setting has not yet been resolved by the caller.
    #[default]
    Workbook,
    Windows1900,
    Mac1904,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExcelSerialDate {
    serial: f64,
    date_system: ExcelDateSystem,
}

impl ExcelSerialDate {
    pub fn new(serial: f64, date_system: ExcelDateSystem) -> XllResult<Self> {
        if !serial.is_finite() {
            return Err(XllError::input("date", InputError::NonFinite));
        }
        Ok(Self {
            serial,
            date_system,
        })
    }

    #[must_use]
    pub const fn serial(self) -> f64 {
        self.serial
    }

    #[must_use]
    pub const fn date_system(self) -> ExcelDateSystem {
        self.date_system
    }

    #[must_use]
    pub const fn with_date_system(mut self, date_system: ExcelDateSystem) -> Self {
        self.date_system = date_system;
        self
    }

    #[must_use]
    pub fn is_fictitious_1900_leap_day(self) -> bool {
        self.date_system == ExcelDateSystem::Windows1900 && self.serial.floor() == 60.0
    }

    #[must_use]
    pub fn fractional_day(self) -> f64 {
        self.serial.rem_euclid(1.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OwnedExcelValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(String),
    Error(ExcelErrorValue),
    Missing,
    Blank,
    Matrix(Matrix<OwnedExcelValue>),
    #[doc(hidden)]
    ArrayOutput(XlArrayOutput),
}

/// An Excel array whose cells are already encoded in their final ABI form.
///
/// Equality compares shape and semantic values for the supported scalar cell
/// types. The numeric builder rejects NaN and infinities, so numeric equality
/// is well-defined.
///
/// Prefer constructing this through [`XlArrayBuilder`]. The return-value layer
/// adopts the cell allocation instead of materializing an intermediate
/// `Vec<OwnedExcelValue>` and encoding it into another array.
/// An Excel array whose cells are already encoded in their final ABI form.
///
/// Equality compares shape and semantic values for the supported scalar cell
/// types. The numeric builder rejects NaN and infinities, so numeric equality
/// is well-defined.
///
/// Prefer constructing this through [`XlArrayBuilder`]. The return-value layer
/// adopts the cell allocation instead of materializing an intermediate
/// `Vec<OwnedExcelValue>` and encoding it into another array.
#[doc(hidden)]
pub struct XlArrayOutput {
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) cells: Box<[XLOPER12]>,
    pub(crate) storage: Option<ReturnStorage>,
    pub(crate) payload_bytes: usize,
}

impl Clone for XlArrayOutput {
    fn clone(&self) -> Self {
        let mut builder = XlArrayBuilder::for_matrix(self.rows, self.columns)
            .expect("validated XlArrayOutput shape");

        for cell in self.cells.iter() {
            builder
                .push_cloned_cell(cell)
                .expect("validated XlArrayOutput cell");
        }

        builder
            .finish()
            .expect("cloning a valid XlArrayOutput must succeed")
    }
}

impl std::fmt::Debug for XlArrayOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("XlArrayOutput")
            .field("rows", &self.rows)
            .field("columns", &self.columns)
            .field("cells", &self.cells.len())
            .finish()
    }
}

fn equal_xloper_cells(left: &XLOPER12, right: &XLOPER12) -> bool {
    let left_type = left.base_type();
    if left_type != right.base_type() {
        return false;
    }

    match left_type {
        XLTYPE_NUM => {
            // SAFETY: XLTYPE_NUM selects the number member.
            unsafe { left.value.number == right.value.number }
        }
        XLTYPE_INT => {
            // SAFETY: XLTYPE_INT selects the integer member.
            unsafe { left.value.integer == right.value.integer }
        }
        XLTYPE_BOOL => {
            // SAFETY: XLTYPE_BOOL selects the boolean member.
            unsafe { left.value.boolean == right.value.boolean }
        }
        XLTYPE_ERR => {
            // SAFETY: XLTYPE_ERR selects the error member.
            unsafe { left.value.error == right.value.error }
        }
        XLTYPE_STR => {
            // SAFETY: XLTYPE_STR selects the counted UTF-16 string member.
            unsafe {
                let left_string = left.value.string;
                let right_string = right.value.string;
                if left_string.is_null() || right_string.is_null() {
                    return left_string.is_null() && right_string.is_null();
                }
                let left_len = *left_string as usize;
                let right_len = *right_string as usize;
                std::slice::from_raw_parts(left_string.add(1), left_len)
                    == std::slice::from_raw_parts(right_string.add(1), right_len)
            }
        }
        XLTYPE_NIL | XLTYPE_MISSING => true,
        // Nested arrays and unknown cell types are outside the output contract.
        _ => false,
    }
}

impl PartialEq for XlArrayOutput {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.columns == other.columns
            && self.cells.len() == other.cells.len()
            && self
                .cells
                .iter()
                .zip(other.cells.iter())
                .all(|(left, right)| equal_xloper_cells(left, right))
    }
}

/// Builds a numeric Excel array directly in its final `XLOPER12` cell buffer.
///
/// This is the low-allocation output path for large calculated arrays. The
/// builder owns exactly one cell buffer; returning the finished value transfers
/// that buffer to the DLL-owned return block without copying its cells.
pub struct XlArrayBuilder {
    rows: usize,
    columns: usize,
    cells: Box<[std::mem::MaybeUninit<XLOPER12>]>,
    initialized: usize,
    storage: Option<ReturnStorage>,
    payload_bytes: usize,
}

impl XlArrayBuilder {
    fn for_matrix(rows: usize, columns: usize) -> XllResult<Self> {
        let len = rows.checked_mul(columns).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;

        validate_matrix_dimensions(rows, columns, len)?;

        let cell_bytes =
            len.checked_mul(std::mem::size_of::<XLOPER12>())
                .ok_or(XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;

        if cell_bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                "<array output>",
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: cell_bytes,
                },
            ));
        }

        Ok(Self {
            rows,
            columns,
            cells: Box::<[XLOPER12]>::new_uninit_slice(len),
            initialized: 0,
            storage: None,
            payload_bytes: cell_bytes,
        })
    }

    pub fn numbers(rows: usize, columns: usize) -> XllResult<Self> {
        Self::for_matrix(rows, columns)
    }

    fn push_oper(&mut self, oper: XLOPER12) -> XllResult<()> {
        if self.initialized == self.cells.len() {
            return Err(XllError::input(
                "<array output>",
                InputError::Malformed("too many array cells"),
            ));
        }

        self.cells[self.initialized].write(oper);
        self.initialized += 1;

        Ok(())
    }

    pub fn push_f64(&mut self, value: f64) -> XllResult<()> {
        if !value.is_finite() {
            return Err(XllError::input("<array output>", InputError::NonFinite));
        }
        self.push_oper(XLOPER12::number(value))
    }

    fn push_string(&mut self, text: String) -> XllResult<()> {
        let utf16_length = crate::utf16::checked_utf16_len(
            &text,
            "<array output>",
            crate::utf16::EXCEL_STRING_LIMIT,
        )?;
        let string_bytes = utf16_length
            .checked_add(1)
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;

        let additional = string_bytes;

        let next_bytes = self
            .payload_bytes
            .checked_add(additional)
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;

        if next_bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                "<array output>",
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: next_bytes,
                },
            ));
        }

        let storage = self.storage.get_or_insert_with(ReturnStorage::new);
        let pointer = storage.alloc_counted_utf16_with_length(
            &text,
            "<array output>",
            crate::utf16::EXCEL_STRING_LIMIT,
            utf16_length,
        )?;
        self.push_oper(XLOPER12 {
            value: xlfn_sys::XLOPER12Value { string: pointer },
            xltype: XLTYPE_STR,
        })?;

        self.payload_bytes = next_bytes;
        Ok(())
    }

    fn push_cloned_cell(&mut self, cell: &XLOPER12) -> XllResult<()> {
        let cell_type = cell.base_type();
        match cell_type {
            XLTYPE_NUM => {
                // SAFETY: XLTYPE_NUM selects the number member.
                unsafe { self.push_oper(XLOPER12::number(cell.value.number)) }
            }
            XLTYPE_INT => {
                // SAFETY: XLTYPE_INT selects the integer member.
                unsafe { self.push_oper(XLOPER12::integer(cell.value.integer)) }
            }
            XLTYPE_BOOL => {
                // SAFETY: XLTYPE_BOOL selects the boolean member.
                unsafe { self.push_oper(XLOPER12::boolean(cell.value.boolean != 0)) }
            }
            XLTYPE_ERR => {
                // SAFETY: XLTYPE_ERR selects the error member.
                unsafe { self.push_oper(XLOPER12::error(cell.value.error)) }
            }
            XLTYPE_NIL => self.push_oper(XLOPER12::nil()),
            XLTYPE_MISSING => self.push_oper(XLOPER12::missing()),
            XLTYPE_STR => {
                // SAFETY: XLTYPE_STR selects string.
                unsafe {
                    let ptr = cell.value.string;
                    if ptr.is_null() {
                        self.push_oper(XLOPER12 {
                            value: xlfn_sys::XLOPER12Value {
                                string: std::ptr::null_mut(),
                            },
                            xltype: XLTYPE_STR,
                        })
                    } else {
                        let len = *ptr as usize;
                        let slice = std::slice::from_raw_parts(ptr.add(1), len);
                        let text = String::from_utf16(slice).map_err(|_| {
                            XllError::input("<array cell>", InputError::InvalidUtf16)
                        })?;
                        self.push_string(text)
                    }
                }
            }
            _ => Err(XllError::input(
                "<array cell>",
                InputError::Malformed("unsupported cell type"),
            )),
        }
    }

    fn push_owned(&mut self, value: OwnedExcelValue) -> XllResult<()> {
        match value {
            OwnedExcelValue::Number(value) if value.is_finite() => {
                self.push_oper(XLOPER12::number(value))
            }
            OwnedExcelValue::Number(_) => Err(XllError::input("<return>", InputError::NonFinite)),
            OwnedExcelValue::Boolean(value) => self.push_oper(XLOPER12::boolean(value)),
            OwnedExcelValue::Integer(value) => self.push_oper(XLOPER12::integer(value)),
            OwnedExcelValue::Error(ExcelErrorValue(error)) => {
                self.push_oper(XLOPER12::error(error.code()))
            }
            OwnedExcelValue::Missing | OwnedExcelValue::Blank => {
                self.push_oper(XLOPER12::error(ExcelError::NotAvailable.code()))
            }
            OwnedExcelValue::String(value) => self.push_string(value),
            OwnedExcelValue::Matrix(_) | OwnedExcelValue::ArrayOutput(_) => Err(XllError::input(
                "<return>",
                InputError::Malformed("nested return arrays are not supported"),
            )),
        }
    }

    pub fn finish(self) -> XllResult<XlArrayOutput> {
        let expected = self.rows * self.columns;

        if self.initialized != expected {
            return Err(XllError::ElementCountMismatch {
                rows: self.rows,
                columns: self.columns,
                expected,
                actual: self.initialized,
            });
        }

        // SAFETY: initialized == cells.len() so every element is written.
        let cells = unsafe { self.cells.assume_init() };

        Ok(XlArrayOutput {
            rows: self.rows,
            columns: self.columns,
            cells,
            storage: self.storage,
            payload_bytes: self.payload_bytes,
        })
    }
}

impl IntoExcelValue for XlArrayOutput {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::ArrayOutput(self))
    }
}

impl<'call> FromExcel<'call> for f64 {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
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
}

impl<'call> FromExcel<'call> for bool {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
        if value.base_type() != XLTYPE_BOOL {
            return Err(value.wrong_type(argument, "boolean"));
        }
        // SAFETY: XLTYPE_BOOL selects the boolean member.
        Ok(unsafe { value.raw.value.boolean } != 0)
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
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
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
}

impl<'call> FromExcel<'call> for i64 {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
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
}

impl<'call> FromExcel<'call> for String {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
        String::from_utf16(value.utf16(argument)?)
            .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))
    }
}

impl<'call, T> FromExcel<'call> for crate::Handle<'call, T>
where
    T: crate::handle::ExcelHandleObject,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let token = String::from_excel(value, argument, context)?;
        context.resolve_handle(&token)
    }
}

impl<'call> FromExcel<'call> for ExcelErrorValue {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        _context: &CallContext<'call>,
    ) -> XllResult<Self> {
        if value.base_type() != XLTYPE_ERR {
            return Err(value.wrong_type(argument, "Excel error"));
        }
        // SAFETY: XLTYPE_ERR selects the error member.
        let code = unsafe { value.raw.value.error };
        ExcelError::from_code(code)
            .map(Self)
            .ok_or_else(|| XllError::input(argument, InputError::Malformed("unknown error code")))
    }
}

impl<'call, T> FromExcel<'call> for OptionalExcelValue<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING => Ok(Self::Missing),
            XLTYPE_NIL => Ok(Self::Blank),
            _ => T::from_excel(value, argument, context).map(Self::Value),
        }
    }
}

impl<'call, T> FromExcel<'call> for Option<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_MISSING | XLTYPE_NIL => Ok(None),
            _ => T::from_excel(value, argument, context).map(Some),
        }
    }
}

impl<'call> FromExcel<'call> for ExcelSerialDate {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        Self::new(
            f64::from_excel(value, argument, context)?,
            ExcelDateSystem::Workbook,
        )
        .map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }
}

impl<'call, T> FromExcel<'call> for Matrix<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
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
            data.push(T::from_excel(element, argument, context)?);
        }
        Self::new(rows, columns, data)
    }
}

impl<'call, T> FromExcel<'call> for Vec<T>
where
    T: FromExcel<'call>,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let matrix = Matrix::<T>::from_excel(value, argument, context)?;
        if matrix.rows() != 1 && matrix.columns() != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: matrix.as_slice().len(),
                },
                actual: Shape {
                    rows: matrix.rows(),
                    columns: matrix.columns(),
                },
            });
        }
        Ok(matrix.into_vec())
    }
}

impl<'call, T, const MAX: usize> FromExcel<'call> for BoundedVarArgs<T, MAX>
where
    T: FromExcel<'call>,
{
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        if MAX == 0 {
            return Err(XllError::input(
                argument,
                InputError::Malformed("bounded varargs maximum must be non-zero"),
            ));
        }
        if value.base_type() == XLTYPE_MULTI {
            let array = value.array(argument)?;
            let rows = array.rows as usize;
            let columns = array.columns as usize;
            let actual = rows.checked_mul(columns).ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array dimension overflow"))
            })?;
            if actual > MAX {
                return Err(XllError::input(
                    argument,
                    InputError::TooLarge { limit: MAX, actual },
                ));
            }
        }
        Self::new(Vec::<T>::from_excel(value, argument, context)?).map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }
}

impl<'call, T: FromExcel<'call>> FromExcel<'call> for Row<T> {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let matrix = Matrix::<T>::from_excel(value, argument, context)?;
        if matrix.rows() != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: matrix.columns(),
                },
                actual: Shape {
                    rows: matrix.rows(),
                    columns: matrix.columns(),
                },
            });
        }
        Ok(Self(matrix.into_vec()))
    }
}

impl<'call, T: FromExcel<'call>> FromExcel<'call> for Column<T> {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        let matrix = Matrix::<T>::from_excel(value, argument, context)?;
        if matrix.columns() != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: matrix.rows(),
                    columns: 1,
                },
                actual: Shape {
                    rows: matrix.rows(),
                    columns: matrix.columns(),
                },
            });
        }
        Ok(Self(matrix.into_vec()))
    }
}

impl<'call> FromExcel<'call> for OwnedExcelValue {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
    ) -> XllResult<Self> {
        match value.base_type() {
            XLTYPE_NUM => f64::from_excel(value, argument, context).map(Self::Number),
            XLTYPE_BOOL => bool::from_excel(value, argument, context).map(Self::Boolean),
            XLTYPE_INT => i32::from_excel(value, argument, context).map(Self::Integer),
            XLTYPE_STR => String::from_excel(value, argument, context).map(Self::String),
            XLTYPE_ERR => ExcelErrorValue::from_excel(value, argument, context).map(Self::Error),
            XLTYPE_MISSING => Ok(Self::Missing),
            XLTYPE_NIL => Ok(Self::Blank),
            XLTYPE_MULTI => {
                Matrix::<OwnedExcelValue>::from_excel(value, argument, context).map(Self::Matrix)
            }
            _ => Err(value.wrong_type(argument, "worksheet value")),
        }
    }
}

impl IntoExcelValue for OwnedExcelValue {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        match self {
            Self::Missing | Self::Blank => Err(XllError::ExcelValue(ExcelError::NotAvailable)),
            Self::Matrix(matrix)
                if matrix
                    .as_slice()
                    .iter()
                    .any(|value| matches!(value, Self::Missing | Self::Blank)) =>
            {
                Err(XllError::ExcelValue(ExcelError::NotAvailable))
            }
            value => Ok(value),
        }
    }
}

impl IntoExcelValue for f64 {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        if self.is_finite() {
            Ok(OwnedExcelValue::Number(self))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            })
        }
    }
}

impl IntoExcelValue for bool {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::Boolean(self))
    }
}

impl IntoExcelValue for i32 {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::Integer(self))
    }
}

impl IntoExcelValue for i64 {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            Ok(OwnedExcelValue::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoExcelValue for ExcelSerialDate {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        self.serial.into_excel_value()
    }
}

impl IntoExcelValue for String {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::String(self))
    }
}

impl IntoExcelValue for &str {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::String(self.to_owned()))
    }
}

macro_rules! direct_excel_returns {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl ExcelReturn for $ty {
                type Output = Self;

                fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
                    Ok(self)
                }
            }

            impl MainThreadReturn for $ty {}
            impl ThreadSafeReturn for $ty {}
            impl MacroSheetReturn for $ty {}
            impl AsyncReturn for $ty {}
            impl VolatileReturn for $ty {}
        )+
    };
}

direct_excel_returns!(
    f64,
    bool,
    i32,
    i64,
    String,
    ExcelErrorValue,
    OwnedExcelValue,
    XlArrayOutput,
    ExcelSerialDate,
);

impl ExcelReturn for &str {
    type Output = Self;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

impl MainThreadReturn for &str {}
impl ThreadSafeReturn for &str {}
impl MacroSheetReturn for &str {}
impl AsyncReturn for &str {}
impl VolatileReturn for &str {}

impl<T: IntoExcelValue> ExcelReturn for Matrix<T> {
    type Output = Self;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

impl<T: IntoExcelValue> MainThreadReturn for Matrix<T> {}
impl<T: IntoExcelValue> ThreadSafeReturn for Matrix<T> {}
impl<T: IntoExcelValue> MacroSheetReturn for Matrix<T> {}
impl<T: IntoExcelValue> AsyncReturn for Matrix<T> {}
impl<T: IntoExcelValue> VolatileReturn for Matrix<T> {}

impl<T: IntoExcelValue> ExcelReturn for Row<T> {
    type Output = Self;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

impl<T: IntoExcelValue> MainThreadReturn for Row<T> {}
impl<T: IntoExcelValue> ThreadSafeReturn for Row<T> {}
impl<T: IntoExcelValue> MacroSheetReturn for Row<T> {}
impl<T: IntoExcelValue> AsyncReturn for Row<T> {}
impl<T: IntoExcelValue> VolatileReturn for Row<T> {}

impl<T: IntoExcelValue> ExcelReturn for Column<T> {
    type Output = Self;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

impl<T: IntoExcelValue> MainThreadReturn for Column<T> {}
impl<T: IntoExcelValue> ThreadSafeReturn for Column<T> {}
impl<T: IntoExcelValue> MacroSheetReturn for Column<T> {}
impl<T: IntoExcelValue> AsyncReturn for Column<T> {}
impl<T: IntoExcelValue> VolatileReturn for Column<T> {}

impl MainThreadReturn for crate::RtdValue {}
impl ThreadSafeReturn for crate::RtdValue {}
impl MacroSheetReturn for crate::RtdValue {}
impl AsyncReturn for crate::RtdValue {}
impl VolatileReturn for crate::RtdValue {}

impl IntoExcelValue for ExcelErrorValue {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Ok(OwnedExcelValue::Error(self))
    }
}

impl<T> IntoExcelValue for Matrix<T>
where
    T: IntoExcelValue,
{
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        let rows = self.rows;
        let columns = self.columns;
        let mut builder = XlArrayBuilder::for_matrix(rows, columns)?;

        for value in self.data {
            let value = value.into_excel_value()?;
            builder.push_owned(value)?;
        }

        Ok(OwnedExcelValue::ArrayOutput(builder.finish()?))
    }
}

impl<T: IntoExcelValue> IntoExcelValue for Row<T> {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Matrix::new(1, self.0.len(), self.0)?.into_excel_value()
    }
}

impl<T: IntoExcelValue> IntoExcelValue for Column<T> {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        Matrix::new(self.0.len(), 1, self.0)?.into_excel_value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use static_assertions::assert_impl_all;
    use xlfn_sys::{XLBIT_XL_FREE, XLOPER12Value};

    assert_impl_all!(
        OwnedExcelValue: std::panic::UnwindSafe, std::panic::RefUnwindSafe
    );
    assert_impl_all!(
        XlArrayBuilder: std::panic::UnwindSafe, std::panic::RefUnwindSafe
    );

    fn convert<T>(raw: &mut XLOPER12) -> XllResult<T>
    where
        T: for<'call> FromExcel<'call>,
    {
        // SAFETY: raw is live for this conversion.
        with_excel_call_scope(|scope| unsafe { argument_from_raw(scope, "arg", raw) })
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
        assert_eq!(value, 4.5);
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
            fn from_excel(
                _value: XlValueRef<'call>,
                _argument: &'static str,
                _context: &CallContext<'call>,
            ) -> XllResult<Self> {
                panic!("element conversion should not occur for oversized inputs");
            }
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
        let values = convert::<Matrix<OwnedExcelValue>>(&mut raw).unwrap();
        assert_eq!(values.as_slice()[0], OwnedExcelValue::Blank);
        assert_eq!(
            values.as_slice()[1],
            OwnedExcelValue::Error(ExcelErrorValue(ExcelError::NotAvailable))
        );
    }

    #[test]
    fn non_finite_values_are_rejected_both_directions() {
        let mut raw = XLOPER12::number(f64::NAN);
        assert!(convert::<f64>(&mut raw).is_err());
        assert!(f64::INFINITY.into_excel_value().is_err());
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
            fn from_excel(
                value: XlValueRef<'call>,
                argument: &'static str,
                context: &CallContext<'call>,
            ) -> XllResult<Self> {
                f64::from_excel(value, argument, context).map(Self)
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
        let mut builder = XlArrayBuilder::numbers(2, 2).unwrap();
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
    fn array_output_equality_uses_the_semantics_of_supported_scalar_cells() {
        let mut left_text = vec![2_u16, b'o' as u16, b'k' as u16];
        let mut right_text = vec![2_u16, b'o' as u16, b'k' as u16];
        let left = XlArrayOutput {
            rows: 1,
            columns: 5,
            cells: vec![
                XLOPER12::number(1.0),
                XLOPER12::integer(2),
                XLOPER12::boolean(true),
                XLOPER12::nil(),
                XLOPER12 {
                    value: XLOPER12Value {
                        string: left_text.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
            ]
            .into_boxed_slice(),
            storage: None,
            payload_bytes: 0,
        };
        let right = XlArrayOutput {
            rows: 1,
            columns: 5,
            cells: vec![
                XLOPER12::number(1.0),
                XLOPER12::integer(2),
                XLOPER12::boolean(true),
                XLOPER12::nil(),
                XLOPER12 {
                    value: XLOPER12Value {
                        string: right_text.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
            ]
            .into_boxed_slice(),
            storage: None,
            payload_bytes: 0,
        };
        assert_eq!(left, right);
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
        let value = matrix.into_excel_value().unwrap();
        assert!(matches!(value, OwnedExcelValue::ArrayOutput(_)));
    }

    #[test]
    fn element_conversion_is_called_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedCell<'a> {
            conversions: &'a AtomicUsize,
            value: f64,
        }

        impl IntoExcelValue for CountedCell<'_> {
            fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
                self.conversions.fetch_add(1, Ordering::Relaxed);
                self.value.into_excel_value()
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
        let _value = matrix.into_excel_value().unwrap();
        assert_eq!(conversions.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn partial_failure_during_matrix_conversion_cleans_up_safely() {
        let data = vec![1.0, 2.0, f64::NAN, 4.0];
        let matrix = Matrix::new(2, 2, data).unwrap();
        let result = matrix.into_excel_value();
        assert!(result.is_err());
    }

    #[test]
    fn array_output_clone_rebases_string_pointers() {
        let mut builder = XlArrayBuilder::for_matrix(1, 1).unwrap();
        builder.push_string("test".to_string()).unwrap();
        let original = builder.finish().unwrap();

        let cloned = original.clone();
        assert_eq!(original, cloned);

        // SAFETY: both original and cloned cells are valid non-null strings.
        unsafe {
            let orig_ptr = original.cells[0].value.string;
            let clone_ptr = cloned.cells[0].value.string;
            assert_ne!(orig_ptr, clone_ptr);
        }
    }
}
