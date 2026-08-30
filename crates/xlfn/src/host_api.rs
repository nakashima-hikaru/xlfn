//! Typed, call-scoped façades over the Excel callback ABI.
//!
//! `HostCallbackSession` owns callback admission and suppression.  This module
//! owns the next protocol layer: mapping an Excel function/status/result triple
//! into a typed `XllResult` while releasing the returned `XLOPER12` exactly
//! once.  Subsystems should use these capabilities for common host operations
//! instead of repeating raw status and cleanup handling.

use crate::callback_value::ExcelCallbackValue;
use crate::error::{ExcelApiFailure, ExcelApiFunction, InputError};
use crate::host_callback::HostCallbackSession;
use crate::reference::ExcelReference;
use crate::return_abi::ExcelCallbackStatus;
use crate::value::{ExcelValue, FromExcel, Matrix, XlValueType, decode_owned_matrix};
use crate::{XllError, XllResult};
use std::ptr::NonNull;
use xlfn_sys::{IDSHEET, XL_COERCE, XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLOPER12};

/// The single-cell caller identity returned by Excel's caller protocol.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HostCaller {
    pub(crate) sheet_id: IDSHEET,
    pub(crate) row: i32,
    pub(crate) column: i32,
}

/// Call-scoped typed host capabilities.
#[derive(Clone, Copy)]
pub(crate) struct ExcelHost<'call> {
    callbacks: &'call HostCallbackSession,
}

/// Result of one host callback after admission, status observation, decoding,
/// and result cleanup have been performed.
///
/// This preserves the evidence needed by mutation-oriented facades without
/// making every ordinary host call understand callback-session details.
pub(crate) enum HostInvocation<T> {
    Suppressed {
        status: ExcelCallbackStatus,
    },
    Completed {
        status: ExcelCallbackStatus,
        decoded: Option<XllResult<T>>,
        cleanup: XllResult<()>,
    },
}

impl<'call> ExcelHost<'call> {
    pub(crate) const fn new(callbacks: &'call HostCallbackSession) -> Self {
        Self { callbacks }
    }

    pub(crate) fn permits_callbacks(&self) -> bool {
        self.callbacks.permits_callbacks()
    }

    pub(crate) fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        self.callbacks.terminal_status()
    }

    /// Executes one callback while preserving status, decode, and cleanup
    /// evidence for callers that need to distinguish rejected from
    /// indeterminate host mutations.
    pub(crate) fn invoke_protocol<T>(
        &self,
        function_id: i32,
        arguments: &[NonNull<XLOPER12>],
        decode: impl FnOnce(&mut ExcelCallbackValue) -> XllResult<T>,
    ) -> HostInvocation<T> {
        // SAFETY: callers provide argument pointers that remain live and
        // stationary for the duration of this callback.
        let (status, mut result) = match unsafe { self.callbacks.call(function_id, arguments) } {
            Ok(call) => call,
            Err(suppressed) => {
                return HostInvocation::Suppressed {
                    status: suppressed.status,
                };
            }
        };

        let decoded = if status == ExcelCallbackStatus::Success {
            Some(decode(&mut result))
        } else {
            None
        };
        let cleanup = result.try_release();
        HostInvocation::Completed {
            status,
            decoded,
            cleanup,
        }
    }

    /// Runs one callback and applies the common status/release protocol.
    ///
    /// The decoder runs while the callback result is still live.  Cleanup is
    /// attempted for both successful and failed decodes; a cleanup failure is
    /// reported when decoding itself succeeded, while a decode error retains
    /// precedence over the cleanup error.
    pub(crate) fn invoke<T>(
        &self,
        function_id: i32,
        function: ExcelApiFunction,
        arguments: &[NonNull<XLOPER12>],
        decode: impl FnOnce(&mut ExcelCallbackValue) -> XllResult<T>,
    ) -> XllResult<T> {
        match self.invoke_protocol(function_id, arguments, decode) {
            HostInvocation::Suppressed { status } => Err(XllError::ExcelApi {
                function,
                failure: ExcelApiFailure::Suppressed(status),
            }),
            HostInvocation::Completed {
                status,
                decoded: _,
                cleanup,
            } if status != ExcelCallbackStatus::Success => {
                Err(cleanup.err().unwrap_or(XllError::ExcelApi {
                    function,
                    failure: ExcelApiFailure::Status(status),
                }))
            }
            HostInvocation::Completed {
                decoded: Some(Err(error)),
                ..
            } => Err(error),
            HostInvocation::Completed {
                decoded: Some(Ok(_value)),
                cleanup: Err(error),
                ..
            } => Err(error),
            HostInvocation::Completed {
                decoded: Some(Ok(value)),
                cleanup: Ok(()),
                ..
            } => Ok(value),
            HostInvocation::Completed { decoded: None, .. } => {
                unreachable!("successful host callbacks always run their decoder")
            }
        }
    }

    pub(crate) fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<ExcelValue> {
        let arguments = [reference.raw_pointer()];
        self.invoke(XL_COERCE, ExcelApiFunction::Coerce, &arguments, |result| {
            <ExcelValue as FromExcel>::from_excel(result.borrow()?, "reference")
        })
    }

    pub(crate) fn coerce_matrix<T>(&self, reference: &ExcelReference<'_>) -> XllResult<Matrix<T>>
    where
        T: for<'value> FromExcel<'value>,
    {
        let arguments = [reference.raw_pointer()];
        self.invoke(XL_COERCE, ExcelApiFunction::Coerce, &arguments, |result| {
            decode_owned_matrix::<T>(result.borrow()?, "reference")
        })
    }

    pub(crate) fn sheet_name(&self, reference: &ExcelReference<'_>) -> XllResult<String> {
        let arguments = [reference.raw_pointer()];
        self.invoke(
            XL_SHEET_NM,
            ExcelApiFunction::SheetName,
            &arguments,
            |result| <String as FromExcel>::from_excel(result.borrow()?, "reference"),
        )
    }

    #[cfg(all(
        target_os = "windows",
        any(feature = "rtd", feature = "handles"),
    ))]
    pub(crate) fn module_path(&self) -> XllResult<String> {
        self.invoke(
            xlfn_sys::XL_GET_NAME,
            ExcelApiFunction::GetName,
            &[],
            |result| <String as FromExcel>::from_excel(result.borrow()?, "module"),
        )
    }

    /// Resolves and validates the single-cell worksheet caller used by handle
    /// formula identities.  The nested sheet-name/sheet-id protocol remains
    /// inside this host capability rather than leaking into handle code.
    pub(crate) fn caller(&self) -> XllResult<HostCaller> {
        self.invoke(XLF_CALLER, ExcelApiFunction::Caller, &[], |caller| {
            let location = {
                let value = caller.borrow()?;
                match value.value_type() {
                    XlValueType::SimpleReference => {
                        // SAFETY: the type selects the SRef member.
                        let reference = unsafe { value.raw().value.sref };
                        if reference.count != 1
                            || reference.reference.rw_first != reference.reference.rw_last
                            || reference.reference.col_first != reference.reference.col_last
                        {
                            return Err(single_cell_caller_error());
                        }
                        CallerLocation::External {
                            row: reference.reference.rw_first,
                            column: reference.reference.col_first,
                        }
                    }
                    XlValueType::Reference => {
                        // SAFETY: the type selects the MRef member.
                        let reference = unsafe { value.raw().value.mref };
                        // SAFETY: Excel supplies a readable reference table
                        // for a well-formed XLTYPE_REF caller value.
                        let table = unsafe { reference.references.as_ref() }
                            .ok_or_else(|| XllError::input("caller", InputError::NullPointer))?;
                        if table.count != 1 {
                            return Err(single_cell_caller_error());
                        }
                        let area = table.reftbl[0];
                        if area.rw_first != area.rw_last || area.col_first != area.col_last {
                            return Err(single_cell_caller_error());
                        }
                        CallerLocation::Direct {
                            sheet_id: reference.sheet_id,
                            row: area.rw_first,
                            column: area.col_first,
                        }
                    }
                    _ => {
                        return Err(XllError::input(
                            "caller",
                            InputError::Malformed(
                                "handle-producing functions require a worksheet caller",
                            ),
                        ));
                    }
                }
            };

            match location {
                CallerLocation::Direct {
                    sheet_id,
                    row,
                    column,
                } => Ok(HostCaller {
                    sheet_id,
                    row,
                    column,
                }),
                CallerLocation::External { row, column } => {
                    // The caller result remains live while Excel resolves its
                    // external sheet identity.
                    let caller_arguments = [caller.raw_pointer()?];
                    let sheet_id = self.invoke(
                        XL_SHEET_NM,
                        ExcelApiFunction::SheetName,
                        &caller_arguments,
                        |sheet| {
                            let sheet_name_arguments = [sheet.raw_pointer()?];
                            self.invoke(
                                XL_SHEET_ID,
                                ExcelApiFunction::SheetId,
                                &sheet_name_arguments,
                                |sheet_id_value| {
                                    let value = sheet_id_value.borrow()?;
                                    if value.value_type() != XlValueType::Reference {
                                        return Err(XllError::input(
                                            "caller",
                                            InputError::Malformed(
                                                "xlSheetId did not return an external reference",
                                            ),
                                        ));
                                    }
                                    // SAFETY: XLTYPE_REF selects the MRef
                                    // member, whose sheet_id is the stable
                                    // worksheet identity returned by Excel.
                                    Ok(unsafe { value.raw().value.mref.sheet_id })
                                },
                            )
                        },
                    )?;
                    Ok(HostCaller {
                        sheet_id,
                        row,
                        column,
                    })
                }
            }
        })
    }
}

enum CallerLocation {
    Direct {
        sheet_id: IDSHEET,
        row: i32,
        column: i32,
    },
    External {
        row: i32,
        column: i32,
    },
}

fn single_cell_caller_error() -> XllError {
    XllError::input(
        "caller",
        InputError::Malformed("handle-producing functions require a single-cell caller"),
    )
}
