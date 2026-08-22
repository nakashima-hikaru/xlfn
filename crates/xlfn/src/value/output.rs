//! Worksheet-output conversion and return dispatch.

use crate::XllResult;
use crate::error::IntoXllError;
use crate::return_value::ReturnContext;

use super::{ExcelCellOutput, ExcelOutput};

/// Converts an ordinary Rust value into a semantic Excel cell.
pub trait IntoExcel {
    fn into_excel(self) -> XllResult<ExcelCellOutput>;
}

/// Framework-side return dispatch used by generated proc-macro code.
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
pub fn assert_main_thread_return<T: MainThreadReturn>() {}

#[doc(hidden)]
pub fn assert_thread_safe_return<T: ThreadSafeReturn>() {}

#[doc(hidden)]
pub fn assert_macro_sheet_return<T: MacroSheetReturn>() {}

#[doc(hidden)]
pub fn assert_async_return<T: AsyncReturn>() {}

#[doc(hidden)]
pub fn assert_volatile_return<T: VolatileReturn>() {}
