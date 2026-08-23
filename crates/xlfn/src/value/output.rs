//! Worksheet-output conversion and return dispatch.

use crate::XllResult;
use crate::error::IntoXllError;
use crate::return_array::XlArrayBuilder;
use crate::return_value::ReturnContext;

use super::input::{InputMode, PlainInputMode};
use super::{ExcelCellOutput, ExcelOutput};

/// Converts an ordinary Rust value into a semantic Excel cell.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be converted into an Excel return value",
    label = "`{Self}` does not implement `IntoExcel`",
    note = "implement `IntoExcel` for `{Self}` or return a supported type (e.g. `f64`, `bool`, `String`, `ExcelError`, or a custom handle)"
)]
pub trait IntoExcel {
    fn into_excel(self) -> XllResult<ExcelCellOutput>;

    /// Encodes directly into an array builder when the value has a primitive
    /// ABI representation. Custom conversions keep the semantic fallback.
    #[doc(hidden)]
    fn push_into(self, builder: &mut XlArrayBuilder) -> XllResult<()>
    where
        Self: Sized,
    {
        builder.push_cell(self.into_excel()?)
    }
}

/// Framework-side return dispatch used by generated proc-macro code.
#[doc(hidden)]
pub trait ExcelReturnSealed {}

/// Framework-side return dispatch used by generated proc-macro code.
#[doc(hidden)]
pub trait ExcelReturn: ExcelReturnSealed + Sized {
    /// Selects the input conversion contract for this return path.
    #[doc(hidden)]
    type InputMode: InputMode;

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
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned from a main-thread Excel worksheet function",
    label = "`{Self}` is not a valid return type for a main-thread UDF",
    note = "return a type that implements `IntoExcel`, or implement `IntoExcel` for `{Self}`"
)]
pub trait MainThreadReturn: ExcelReturn {}

/// Return values supported by Excel multi-threaded recalculation.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned from a thread-safe Excel worksheet function",
    label = "`{Self}` is not a valid return type for a thread-safe UDF",
    note = "return a type that implements `IntoExcel`, or implement `IntoExcel` for `{Self}`"
)]
pub trait ThreadSafeReturn: ExcelReturn {}

/// Return values supported by macro-sheet functions.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned from a macro-sheet Excel function",
    label = "`{Self}` is not a valid return type for a macro-sheet function",
    note = "return a type that implements `IntoExcel`, or implement `IntoExcel` for `{Self}`"
)]
pub trait MacroSheetReturn: ExcelReturn {}

/// Return values supported by native asynchronous functions.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned from an asynchronous Excel function",
    label = "`{Self}` is not a valid return type for an async UDF",
    note = "return a type that implements `IntoExcel`, or implement `IntoExcel` for `{Self}`"
)]
pub trait AsyncReturn: ExcelReturn {}

/// Return values supported by volatile functions.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned from a volatile Excel worksheet function",
    label = "`{Self}` is not a valid return type for a volatile UDF",
    note = "return a type that implements `IntoExcel`, or implement `IntoExcel` for `{Self}`"
)]
pub trait VolatileReturn: ExcelReturn {}

impl<T: IntoExcel> ExcelReturn for T {
    type InputMode = PlainInputMode;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ExcelOutput> {
        IntoExcel::into_excel(self).map(ExcelOutput::Scalar)
    }
}

impl<T: IntoExcel> ExcelReturnSealed for T {}

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
    type InputMode = T::InputMode;

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

impl<T, E> ExcelReturnSealed for Result<T, E>
where
    T: ExcelReturn,
    E: IntoXllError,
{
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
pub fn assert_main_thread_return<T: MainThreadReturn>() {}

#[doc(hidden)]
pub fn assert_thread_safe_return<T: ThreadSafeReturn>() {}

#[doc(hidden)]
pub fn assert_macro_sheet_return<T: MacroSheetReturn>() {}

#[doc(hidden)]
pub fn assert_async_return<T: AsyncReturn>() {}

#[doc(hidden)]
pub fn assert_volatile_return<T: VolatileReturn>() {}
