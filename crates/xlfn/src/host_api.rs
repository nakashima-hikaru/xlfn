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
use crate::value::{ExcelDateSystem, ExcelValue, FromExcel, Matrix, convert};
#[cfg(feature = "handles")]
use crate::value::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};
use std::ptr::NonNull;
#[cfg(feature = "handles")]
use xlfn_sys::{IDSHEET, XL_SHEET_ID, XLF_CALLER};
use xlfn_sys::{XL_COERCE, XL_SHEET_NM, XLF_GET_DOCUMENT, XLOPER12};

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

    /// Uses an explicit sheet identity to name the workbook for GET.DOCUMENT.
    /// Never omits its workbook operand or resolves an implicit current sheet.
    pub(crate) fn workbook_date_system(
        &self,
        reference: &ExcelReference<'_>,
    ) -> XllResult<ExcelDateSystem> {
        if reference.sheet_id().is_none_or(|sheet| sheet.get() == 0) {
            return Err(XllError::input(
                "reference",
                crate::error::InputError::Malformed(
                    "workbook date system requires an explicit nonzero sheet identity",
                ),
            ));
        }
        let sheet = self.sheet_name(reference)?;
        let workbook = workbook_name(&sheet)?;
        let mut units =
            crate::utf16::encode_counted(workbook, "reference", crate::utf16::EXCEL_STRING_LIMIT)?;
        let mut name = XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: units.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        // GET.DOCUMENT(20, workbook) is TRUE for the 1904 date system.
        let mut selector = XLOPER12::integer(20);
        let arguments = [
            NonNull::from_mut(&mut selector),
            NonNull::from_mut(&mut name),
        ];
        self.invoke(
            XLF_GET_DOCUMENT,
            ExcelApiFunction::GetDocument,
            &arguments,
            |result| {
                let value = result.borrow()?;
                match value.value_type() {
                    crate::value::XlValueType::Boolean => {
                        Ok(if <bool as FromExcel>::from_excel(value, "reference")? {
                            ExcelDateSystem::Mac1904
                        } else {
                            ExcelDateSystem::Windows1900
                        })
                    }
                    crate::value::XlValueType::Error => Err(XllError::ExcelValue(
                        <crate::ExcelError as FromExcel>::from_excel(value, "reference")?,
                    )),
                    _ => Err(XllError::ExcelApi {
                        function: ExcelApiFunction::GetDocument,
                        failure: ExcelApiFailure::UnexpectedResult,
                    }),
                }
            },
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

fn workbook_name(sheet: &str) -> XllResult<&str> {
    let name = sheet
        .strip_prefix('[')
        .and_then(|name| name.split_once(']'));
    match name {
        Some((workbook, sheet))
            if !workbook.is_empty() && !sheet.is_empty() && !workbook.contains(['[', '\0']) =>
        {
            Ok(workbook)
        }
        _ => Err(XllError::ExcelApi {
            function: ExcelApiFunction::SheetName,
            failure: ExcelApiFailure::UnexpectedResult,
        }),
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

#[cfg(test)]
mod date_tests {
    use super::*;
    use crate::reference::FromExcelReference;
    use crate::registration::host::test_support::{
        CallbackScript, RegisterArgument, Reply, read_registration_arguments,
    };
    use crate::value::XlValueRef;
    use xlfn_sys::{XLMREF12, XLOPER12MRef, XLOPER12SRef, XLOPER12Value, XLREF12};

    fn area() -> XLREF12 {
        XLREF12 {
            rw_first: 0,
            rw_last: 0,
            col_first: 0,
            col_last: 0,
        }
    }

    fn external(table: &mut XLMREF12, sheet_id: usize) -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                mref: XLOPER12MRef {
                    references: table,
                    sheet_id,
                },
            },
            xltype: xlfn_sys::XLTYPE_REF,
        }
    }

    fn reference(raw: &XLOPER12) -> ExcelReference<'_> {
        ExcelReference::from_excel_reference(XlValueRef::from_array_cell(raw).unwrap(), "reference")
            .unwrap()
    }

    fn text(units: &mut [u16]) -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                string: units.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        }
    }

    #[test]
    fn workbook_date_system_names_the_referenced_workbook_on_every_query() {
        // Keep one callback authority while alternating workbook identities,
        // then changing the first workbook's setting without changing its ID.
        let callbacks = HostCallbackSession::new();
        let history = crate::diagnostics::DiagnosticHistory::new();
        let context =
            crate::MacroSheetContext::<()>::new(&(), &history, ExcelHost::new(&callbacks));
        for (sheet_id, workbook, mac) in [
            (42, "Book1900.xlsx", false),
            (91, "価格 '💡.xlsx", true),
            (42, "Book1900.xlsx", true),
        ] {
            let mut table = XLMREF12 {
                count: 1,
                reftbl: [area()],
            };
            let raw = external(&mut table, sheet_id);
            let mut sheet_name =
                crate::utf16::encode_counted(&format!("[{workbook}]Data"), "test", 32767).unwrap();
            let script = CallbackScript::install([
                Reply::success(XL_SHEET_NM, text(&mut sheet_name)),
                Reply::success(XLF_GET_DOCUMENT, XLOPER12::boolean(mac)),
            ]);
            script.on_arguments(XL_SHEET_NM, move |arguments| {
                assert_eq!(arguments.len(), 1);
                // SAFETY: the script borrows the live reference operand.
                let raw = unsafe { &*arguments[0] };
                assert_eq!(raw.base_type(), xlfn_sys::XLTYPE_REF);
                // SAFETY: the external-reference tag selects mref.
                assert_eq!(unsafe { raw.value.mref.sheet_id }, sheet_id);
            });
            script.on_arguments(XLF_GET_DOCUMENT, move |arguments| {
                assert_eq!(arguments.len(), 2);
                // SAFETY: the script borrows both live callback operands.
                let selector = unsafe { &*arguments[0] };
                assert_eq!(selector.base_type(), xlfn_sys::XLTYPE_INT);
                // SAFETY: the integer tag selects the selector union member.
                assert_eq!(unsafe { selector.value.integer }, 20);
                // SAFETY: the second operand is the live counted workbook name.
                let names = unsafe { read_registration_arguments(&arguments[1..]) };
                assert_eq!(names, [RegisterArgument::Text(workbook.to_owned())]);
            });
            assert_eq!(
                context.workbook_date_system(&reference(&raw)).unwrap(),
                if mac {
                    ExcelDateSystem::Mac1904
                } else {
                    ExcelDateSystem::Windows1900
                }
            );
            script.assert_calls(&[XL_SHEET_NM, XLF_GET_DOCUMENT]);
        }
    }

    #[test]
    fn workbook_date_system_rejects_ambiguous_sheet_identity_without_callbacks() {
        let mut table = XLMREF12 {
            count: 1,
            reftbl: [area()],
        };
        let roots = [
            external(&mut table, 0),
            XLOPER12 {
                value: XLOPER12Value {
                    sref: XLOPER12SRef {
                        count: 1,
                        reference: area(),
                    },
                },
                xltype: xlfn_sys::XLTYPE_SREF,
            },
        ];
        let script = CallbackScript::install([]);
        let callbacks = HostCallbackSession::new();
        for raw in &roots {
            assert!(matches!(
                ExcelHost::new(&callbacks).workbook_date_system(&reference(raw)),
                Err(XllError::Input {
                    argument: "reference",
                    ..
                })
            ));
        }
        script.assert_calls(&[]);
    }

    #[test]
    fn workbook_date_system_rejects_unknown_settings_without_coercion_or_default() {
        let mut table = XLMREF12 {
            count: 1,
            reftbl: [area()],
        };
        let raw = external(&mut table, 42);
        for result in [
            XLOPER12::number(0.0),
            XLOPER12::integer(1),
            XLOPER12::nil(),
            XLOPER12::error(xlfn_sys::XLERR_REF),
        ] {
            let mut sheet = crate::utf16::encode_counted("[Book.xlsx]Data", "test", 32767).unwrap();
            let script = CallbackScript::install([
                Reply::success(XL_SHEET_NM, text(&mut sheet)),
                Reply::success(XLF_GET_DOCUMENT, result),
            ]);
            let callbacks = HostCallbackSession::new();
            let error = ExcelHost::new(&callbacks)
                .workbook_date_system(&reference(&raw))
                .unwrap_err();
            if result.base_type() == xlfn_sys::XLTYPE_ERR {
                assert!(matches!(
                    error,
                    XllError::ExcelValue(crate::ExcelError::Reference)
                ));
            } else {
                assert!(matches!(
                    error,
                    XllError::ExcelApi {
                        function: ExcelApiFunction::GetDocument,
                        failure: ExcelApiFailure::UnexpectedResult
                    }
                ));
            }
            script.assert_calls(&[XL_SHEET_NM, XLF_GET_DOCUMENT]);
        }
    }

    #[test]
    fn workbook_date_system_does_not_fall_back_after_malformed_sheet_names() {
        let mut table = XLMREF12 {
            count: 1,
            reftbl: [area()],
        };
        let raw = external(&mut table, 42);
        for malformed in [
            "",
            "Data",
            "[]Data",
            "[Book]",
            "[[Book]Data",
            "[Book\0]Data",
        ] {
            let mut sheet = crate::utf16::encode_counted(malformed, "test", 32767).unwrap();
            let script = CallbackScript::install([Reply::success(XL_SHEET_NM, text(&mut sheet))]);
            let callbacks = HostCallbackSession::new();
            assert!(
                ExcelHost::new(&callbacks)
                    .workbook_date_system(&reference(&raw))
                    .is_err()
            );
            script.assert_calls(&[XL_SHEET_NM]);
        }
    }

    #[test]
    fn workbook_date_system_propagates_callback_rejection_and_terminal_suppression() {
        let mut table = XLMREF12 {
            count: 1,
            reftbl: [area()],
        };
        let raw = external(&mut table, 42);
        for function in [XL_SHEET_NM, XLF_GET_DOCUMENT] {
            for status in [
                xlfn_sys::XLRET_FAILED,
                xlfn_sys::XLRET_ABORT,
                xlfn_sys::XLRET_UNCALCED,
            ] {
                let mut sheet = crate::utf16::encode_counted("[Book]Data", "test", 32767).unwrap();
                let mut replies = Vec::new();
                if function == XLF_GET_DOCUMENT {
                    replies.push(Reply::success(XL_SHEET_NM, text(&mut sheet)));
                }
                replies.push(Reply::success(function, XLOPER12::nil()).status(status));
                let script = CallbackScript::install(replies);
                let callbacks = HostCallbackSession::new();
                let host = ExcelHost::new(&callbacks);
                assert!(
                    matches!(host.workbook_date_system(&reference(&raw)), Err(XllError::ExcelApi {
                    failure: ExcelApiFailure::Status(actual), ..
                }) if actual == ExcelCallbackStatus::from_raw(status))
                );
                if status != xlfn_sys::XLRET_FAILED {
                    assert!(matches!(
                        host.workbook_date_system(&reference(&raw)),
                        Err(XllError::ExcelApi {
                            failure: ExcelApiFailure::Suppressed(_),
                            ..
                        })
                    ));
                }
                script.assert_calls(if function == XL_SHEET_NM {
                    &[XL_SHEET_NM]
                } else {
                    &[XL_SHEET_NM, XLF_GET_DOCUMENT]
                });
            }
        }
    }

    #[test]
    fn workbook_date_system_returns_cleanup_failure_instead_of_a_setting() {
        let mut table = XLMREF12 {
            count: 1,
            reftbl: [area()],
        };
        let raw = external(&mut table, 42);
        let mut sheet = crate::utf16::encode_counted("[Book]Data", "test", 32767).unwrap();
        let script = CallbackScript::install([
            Reply::success(XL_SHEET_NM, text(&mut sheet)),
            Reply::success(XLF_GET_DOCUMENT, XLOPER12::boolean(false))
                .release_status(xlfn_sys::XLRET_FAILED),
        ]);
        let callbacks = HostCallbackSession::new();
        assert!(matches!(
            ExcelHost::new(&callbacks).workbook_date_system(&reference(&raw)),
            Err(XllError::ExcelApi {
                function: ExcelApiFunction::Free,
                ..
            })
        ));
        script.assert_calls(&[XL_SHEET_NM, XLF_GET_DOCUMENT]);
    }
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
