//! Typed, call-scoped façades over the Excel callback ABI.
//!
//! `HostCallbackSession` owns callback admission and suppression.  This module
//! owns the next protocol layer: mapping an Excel function/status/result triple
//! into a typed `XllResult` while releasing the returned `XLOPER12` exactly
//! once.  Subsystems should use these capabilities for common host operations
//! instead of repeating raw status and cleanup handling.

use crate::callback_value::ExcelCallbackValue;
#[cfg(feature = "handles")]
use crate::error::InputError;
use crate::error::{ExcelApiFailure, ExcelApiFunction};
use crate::host_callback::HostCallbackSession;
use crate::reference::ExcelReference;
#[cfg(feature = "handles")]
use crate::reference::FromExcelReference;
use crate::return_abi::ExcelCallbackStatus;
use crate::value::{ExcelValue, FromExcel, Matrix, convert};
#[cfg(feature = "handles")]
use crate::value::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};
use std::ptr::NonNull;
#[cfg(feature = "handles")]
use xlfn_sys::{IDSHEET, XL_SHEET_ID, XLF_CALLER};
use xlfn_sys::{XL_COERCE, XL_SHEET_NM, XLOPER12};

/// The single-cell caller identity returned by Excel's caller protocol.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg(feature = "handles")]
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
        decode: impl FnOnce(&mut ExcelCallbackValue<'_>) -> XllResult<T>,
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
        decode: impl FnOnce(&mut ExcelCallbackValue<'_>) -> XllResult<T>,
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
            convert::matrix(result.borrow()?, "reference", T::from_excel)
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

    #[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles"),))]
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
    #[cfg(feature = "handles")]
    pub(crate) fn caller(&self) -> XllResult<HostCaller> {
        self.invoke(XLF_CALLER, ExcelApiFunction::Caller, &[], |caller| {
            let location = caller_location(caller.borrow()?)?;

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

#[cfg(feature = "handles")]
#[derive(Debug, PartialEq)]
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

#[cfg(feature = "handles")]
fn caller_location(value: XlValueRef<'_>) -> XllResult<CallerLocation> {
    if !matches!(
        value.value_type(),
        XlValueType::SimpleReference | XlValueType::Reference
    ) {
        return Err(XllError::input(
            "caller",
            InputError::Malformed("handle-producing functions require a worksheet caller"),
        ));
    }
    let reference = ExcelReference::from_excel_reference(value, "caller")?;
    let mut areas = reference.areas();
    let area = areas.next().ok_or_else(single_cell_caller_error)?;
    if areas.next().is_some() || area.row_count() != 1 || area.column_count() != 1 {
        return Err(single_cell_caller_error());
    }
    // Validated worksheet coordinates fit i32 on every supported target.
    let row = area.first_row() as i32;
    let column = area.first_column() as i32;
    Ok(match reference.sheet_id() {
        Some(sheet_id) => CallerLocation::Direct {
            sheet_id: sheet_id.get(),
            row,
            column,
        },
        None => CallerLocation::External { row, column },
    })
}

#[cfg(feature = "handles")]
fn single_cell_caller_error() -> XllError {
    XllError::input(
        "caller",
        InputError::Malformed("handle-producing functions require a single-cell caller"),
    )
}

#[cfg(all(test, feature = "handles"))]
mod tests {
    use super::*;
    use xlfn_sys::{XLMREF12, XLOPER12MRef, XLOPER12SRef, XLOPER12Value, XLREF12};

    #[test]
    fn caller_reference_validation_checks_sheet_bounds_for_both_reference_kinds() {
        for (row, column) in [(-1, 0), (0, -1), (1_048_576, 0), (0, 16_384)] {
            let area = XLREF12 {
                rw_first: row,
                rw_last: row,
                col_first: column,
                col_last: column,
            };
            let mut table = XLMREF12 {
                count: 1,
                reftbl: [area],
            };
            let roots = [
                XLOPER12 {
                    value: XLOPER12Value {
                        sref: XLOPER12SRef {
                            count: 1,
                            reference: area,
                        },
                    },
                    xltype: xlfn_sys::XLTYPE_SREF,
                },
                XLOPER12 {
                    value: XLOPER12Value {
                        mref: XLOPER12MRef {
                            references: &mut table,
                            sheet_id: 42,
                        },
                    },
                    xltype: xlfn_sys::XLTYPE_REF,
                },
            ];
            for raw in &roots {
                let value = XlValueRef::from_array_cell(raw).unwrap();
                assert!(matches!(
                    caller_location(value),
                    Err(XllError::Input {
                        argument: "caller",
                        reason: InputError::Malformed("invalid reference area"),
                    })
                ));
            }
        }
    }

    #[test]
    fn miri_caller_rejects_misaligned_reference_tables_before_reading() {
        #[repr(align(8))]
        struct Buffer([u8; 32]);
        let mut buffer = Buffer([0; 32]);
        let raw = XLOPER12 {
            value: XLOPER12Value {
                mref: XLOPER12MRef {
                    references: buffer.0.as_mut_ptr().wrapping_add(1).cast(),
                    sheet_id: 42,
                },
            },
            xltype: xlfn_sys::XLTYPE_REF,
        };
        let value = XlValueRef::from_array_cell(&raw).unwrap();
        assert!(matches!(
            caller_location(value),
            Err(XllError::Input {
                argument: "caller",
                reason: InputError::Malformed("misaligned reference table pointer"),
            })
        ));
    }
}
