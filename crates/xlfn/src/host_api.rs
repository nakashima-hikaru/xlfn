//! Typed, call-scoped façades over the Excel callback ABI.
//!
//! `HostCallbackSession` owns callback admission and suppression.  This module
//! owns the next protocol layer: mapping an Excel function/status/result triple
//! into a typed `XllResult` while releasing the returned `XLOPER12` exactly
//! once.  Subsystems should use these capabilities for common host operations
//! instead of repeating raw status and cleanup handling.

use crate::callback_value::{CallbackArguments, CallbackResult, ExcelCallbackValue};
#[cfg(feature = "handles")]
use crate::error::InputError;
use crate::error::{ExcelApiFailure, ExcelApiFunction};
use crate::host_callback::{HostCallbackSession, HostCallbackSuppressed};
use crate::reference::ExcelReference;
#[cfg(feature = "handles")]
use crate::reference::FromExcelReference;
use crate::return_abi::ExcelCallbackStatus;
#[cfg(feature = "handles")]
use crate::value::XlValueType;
use crate::value::{ExcelDateSystem, ExcelValue, FromExcel, Matrix, XlValueRef, convert};
use crate::{XllError, XllResult};
use smallvec::SmallVec;
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
    Decoded {
        decoded: XllResult<T>,
        cleanup: XllResult<()>,
    },
    Failed {
        status: HostFailureStatus,
        cleanup: XllResult<()>,
    },
}

/// Only the callback issuer can classify an invoked, unsuccessful callback.
/// A successful callback instead carries its mandatory read/decode outcome.
pub(crate) struct HostFailureStatus(ExcelCallbackStatus);

impl HostFailureStatus {
    pub(crate) const fn status(&self) -> ExcelCallbackStatus {
        self.0
    }
}

fn finish_invocation<T>(
    call: Result<(ExcelCallbackStatus, CallbackResult<'_>), HostCallbackSuppressed>,
    decode: impl FnOnce(&mut ExcelCallbackValue<'_>) -> XllResult<T>,
) -> HostInvocation<T> {
    let (status, mut result) = match call {
        Ok(call) => call,
        Err(suppressed) => {
            return HostInvocation::Suppressed {
                status: suppressed.status,
            };
        }
    };

    if status == ExcelCallbackStatus::Success {
        let decoded = result.readable().and_then(decode);
        let cleanup = result.release();
        HostInvocation::Decoded { decoded, cleanup }
    } else {
        let cleanup = result.release();
        HostInvocation::Failed {
            status: HostFailureStatus(status),
            cleanup,
        }
    }
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
        finish_invocation(
            unsafe { self.callbacks.call(function_id, arguments) },
            decode,
        )
    }

    /// Like invoke_protocol, but the caller lends Excel its own mutable
    /// pointer array instead of copying it at the host boundary.
    ///
    /// # Safety
    ///
    /// Operands must be live, stationary and read-only until the callback and
    /// result release finish. Excel may overwrite the scratch pointers.
    pub(crate) unsafe fn invoke_protocol_with_scratch<T>(
        &self,
        function_id: i32,
        scratch: &mut [*mut XLOPER12],
        decode: impl FnOnce(&mut ExcelCallbackValue<'_>) -> XllResult<T>,
    ) -> HostInvocation<T> {
        // SAFETY: this method's caller supplies the live, stationary operands.
        let call = unsafe {
            self.callbacks
                .call_with_arguments(function_id, CallbackArguments::Scratch(scratch))
        };
        finish_invocation(call, decode)
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
            HostInvocation::Suppressed { status } => Err(crate::error::construct::excel_api(
                function,
                ExcelApiFailure::Suppressed(status),
            )),
            HostInvocation::Failed { status, cleanup } => {
                Err(cleanup.err().unwrap_or(crate::error::construct::excel_api(
                    function,
                    ExcelApiFailure::Status(status.status()),
                )))
            }
            HostInvocation::Decoded {
                decoded: Err(error),
                ..
            } => Err(error),
            HostInvocation::Decoded {
                decoded: Ok(_value),
                cleanup: Err(error),
            } => Err(error),
            HostInvocation::Decoded {
                decoded: Ok(value),
                cleanup: Ok(()),
            } => Ok(value),
        }
    }

    /// Invokes xlCoerce and visits the result while Excel still owns it.
    /// The visitor cannot return a borrow of the callback result.
    pub(crate) fn coerce_with<R>(
        &self,
        reference: &ExcelReference<'_>,
        visit: impl for<'value> FnOnce(XlValueRef<'value>) -> XllResult<R>,
    ) -> XllResult<R> {
        let arguments = [reference.raw_pointer()];
        self.invoke(XL_COERCE, ExcelApiFunction::Coerce, &arguments, |result| {
            visit(result.borrow()?)
        })
    }

    pub(crate) fn coerce(&self, reference: &ExcelReference<'_>) -> XllResult<ExcelValue> {
        self.coerce_with(reference, |value| {
            <ExcelValue as FromExcel>::from_excel(value, "reference")
        })
    }

    pub(crate) fn coerce_matrix<T>(&self, reference: &ExcelReference<'_>) -> XllResult<Matrix<T>>
    where
        T: for<'value> FromExcel<'value>,
    {
        self.coerce_with(reference, |value| {
            convert::matrix(value, "reference", T::from_excel)
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
        // Preserve the xlSheetNm result's cleanup boundary before issuing
        // GET.DOCUMENT. Only the counted workbook operand is retained.
        let sheet_arguments = [reference.raw_pointer()];
        let mut units = self.invoke(
            XL_SHEET_NM,
            ExcelApiFunction::SheetName,
            &sheet_arguments,
            |result| workbook_name_counted(result.borrow()?.utf16("reference")?),
        )?;
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
                    crate::value::XlValueType::Error => Err(XllError::excel_value(
                        <crate::ExcelError as FromExcel>::from_excel(value, "reference")?,
                    )),
                    _ => Err(crate::error::construct::excel_api(
                        ExcelApiFunction::GetDocument,
                        ExcelApiFailure::UnexpectedResult,
                    )),
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
                    let caller_arguments = [caller.raw_pointer()];
                    let sheet_id = self.invoke(
                        XL_SHEET_NM,
                        ExcelApiFunction::SheetName,
                        &caller_arguments,
                        |sheet| {
                            let sheet_name_arguments = [sheet.raw_pointer()];
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
                                    let sheet_id = unsafe { value.raw().value.mref.sheet_id };
                                    if sheet_id == 0 {
                                        return Err(crate::error::construct::excel_api(
                                            ExcelApiFunction::SheetId,
                                            ExcelApiFailure::UnexpectedResult,
                                        ));
                                    }
                                    Ok(sheet_id)
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

/// Extract the workbook operand directly from the callback's UTF-16 result.
fn workbook_name_counted(sheet: &[u16]) -> XllResult<SmallVec<[u16; 64]>> {
    // The previous String decoder validated the entire sheet name, including
    // the suffix that is not passed to GET.DOCUMENT. Preserve that behavior.
    if char::decode_utf16(sheet.iter().copied()).any(|unit| unit.is_err()) {
        return Err(XllError::input(
            "reference",
            crate::error::InputError::InvalidUtf16,
        ));
    }
    let close = sheet.iter().position(|&unit| unit == u16::from(b']'));
    let workbook = close
        .filter(|&index| {
            sheet.first() == Some(&u16::from(b'[')) && index > 1 && index + 1 < sheet.len()
        })
        .map(|index| &sheet[1..index])
        .filter(|name| !name.contains(&u16::from(b'[')) && !name.contains(&0))
        .ok_or(crate::error::construct::excel_api(
            ExcelApiFunction::SheetName,
            ExcelApiFailure::UnexpectedResult,
        ))?;

    // The source is an Excel counted string, so the substring fits the same
    // 16-bit length bound. Small workbook names stay inside SmallVec.
    let mut units = SmallVec::with_capacity(workbook.len() + 1);
    units.push(workbook.len() as u16);
    units.extend_from_slice(workbook);
    Ok(units)
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
    // Excel treats a zero id like an internal reference to the current sheet.
    Ok(match reference.sheet_id().filter(|id| id.get() != 0) {
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

    #[test]
    fn successful_invocation_carries_one_mandatory_decoder_result() {
        let script = CallbackScript::install([Reply::success(
            xlfn_sys::XLF_EVALUATE,
            XLOPER12::number(7.0),
        )]);
        let callbacks = HostCallbackSession::new();
        let decodes = std::cell::Cell::new(0);
        let result =
            ExcelHost::new(&callbacks).invoke_protocol(xlfn_sys::XLF_EVALUATE, &[], |_| {
                decodes.set(decodes.get() + 1);
                Ok(7_u32)
            });
        assert!(matches!(
            result,
            HostInvocation::Decoded {
                decoded: Ok(7),
                cleanup: Ok(())
            }
        ));
        assert_eq!(decodes.get(), 1);
        script.assert_calls(&[xlfn_sys::XLF_EVALUATE]);
    }

    #[test]
    fn failed_invocation_cannot_run_a_decoder_and_terminal_status_still_suppresses() {
        for status in [xlfn_sys::XLRET_FAILED, xlfn_sys::XLRET_ABORT] {
            let script = CallbackScript::install([Reply::success(
                xlfn_sys::XLF_EVALUATE,
                XLOPER12::number(7.0),
            )
            .status(status)]);
            let callbacks = HostCallbackSession::new();
            let host = ExcelHost::new(&callbacks);
            let result: HostInvocation<()> =
                host.invoke_protocol(xlfn_sys::XLF_EVALUATE, &[], |_| {
                    panic!("an unsuccessful callback has no readable result")
                });
            assert!(matches!(result, HostInvocation::Failed {
                status: ref observed, cleanup: Ok(())
            } if observed.status() == crate::error::callback_status_from_raw(status)));
            if status == xlfn_sys::XLRET_ABORT {
                let suppressed: HostInvocation<()> =
                    host.invoke_protocol(xlfn_sys::XLF_EVALUATE, &[], |_| {
                        panic!("a suppressed callback has no result")
                    });
                assert!(matches!(
                    suppressed,
                    HostInvocation::Suppressed {
                        status: ExcelCallbackStatus::Abort
                    }
                ));
            }
            script.assert_calls(&[xlfn_sys::XLF_EVALUATE]);
        }
    }

    #[test]
    fn miri_decoder_panic_still_frees_host_result_once() {
        let script = CallbackScript::install([Reply::success(
            xlfn_sys::XLF_EVALUATE,
            XLOPER12::number(7.0),
        )]);
        let callbacks = HostCallbackSession::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: HostInvocation<()> =
                ExcelHost::new(&callbacks)
                    .invoke_protocol(xlfn_sys::XLF_EVALUATE, &[], |_| panic!("decoder panic"));
        }));
        assert!(result.is_err());
        script.assert_calls(&[xlfn_sys::XLF_EVALUATE]);
    }

    #[test]
    fn decoder_error_keeps_precedence_over_result_cleanup_failure() {
        let script = CallbackScript::install([Reply::success(
            xlfn_sys::XLF_EVALUATE,
            XLOPER12::number(7.0),
        )
        .release_status(xlfn_sys::XLRET_FAILED)]);
        let callbacks = HostCallbackSession::new();
        let result: XllResult<()> = ExcelHost::new(&callbacks).invoke(
            xlfn_sys::XLF_EVALUATE,
            ExcelApiFunction::Evaluate,
            &[],
            |_| Err(XllError::CLOSING),
        );
        assert!(matches!(result, Err(__err) if __err.is_closing()));
        script.assert_calls(&[xlfn_sys::XLF_EVALUATE]);
    }

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
    fn workbook_utf16_parser_matches_owned_sheet_name_protocol() {
        for (sheet, expected) in [
            ("[Book.xlsx]Data", Some("Book.xlsx")),
            ("[価格💡.xlsx]Sheet", Some("価格💡.xlsx")),
            ("", None),
            ("Data", None),
            ("[]Data", None),
            ("[Book]", None),
            ("[[Book]Data", None),
            ("[Book\0]Data", None),
        ] {
            let units = sheet.encode_utf16().collect::<Vec<_>>();
            let parsed = workbook_name_counted(&units);
            match expected {
                Some(name) => {
                    let counted = parsed.unwrap();
                    assert_eq!(usize::from(counted[0]), counted.len() - 1);
                    assert!(counted[1..].iter().copied().eq(name.encode_utf16()));
                }
                None => assert!(matches!(
                    (parsed).as_ref().map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::ExcelApi {
                        function: ExcelApiFunction::SheetName,
                        failure: ExcelApiFailure::UnexpectedResult,
                        ..
                    })
                )),
            }
        }
        let mut malformed = "[Book.xlsx]Data".encode_utf16().collect::<Vec<_>>();
        malformed.push(0xd800);
        assert!(matches!(
            (workbook_name_counted(&malformed))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "reference",
                reason: crate::error::InputError::InvalidUtf16,
            })
        ));
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
                (ExcelHost::new(&callbacks).workbook_date_system(&reference(raw)))
                    .as_ref()
                    .map_err(crate::error::construct::view),
                Err(crate::error::construct::ErrorView::Input {
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
                    __err if __err.excel_value_error() == Some(crate::ExcelError::Reference)
                ));
            } else {
                assert!(matches!(
                    crate::error::construct::view(&(error)),
                    crate::error::construct::ErrorView::ExcelApi {
                        function: ExcelApiFunction::GetDocument,
                        failure: ExcelApiFailure::UnexpectedResult,
                        ..
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
                    matches!((host.workbook_date_system(&reference(&raw))).as_ref().map_err(crate::error::construct::view), Err(crate::error::construct::ErrorView::ExcelApi {
                    failure: ExcelApiFailure::Status(actual), ..
                }) if actual == crate::error::callback_status_from_raw(status))
                );
                if status != xlfn_sys::XLRET_FAILED {
                    assert!(matches!(
                        (host.workbook_date_system(&reference(&raw)))
                            .as_ref()
                            .map_err(crate::error::construct::view),
                        Err(crate::error::construct::ErrorView::ExcelApi {
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
            (ExcelHost::new(&callbacks).workbook_date_system(&reference(&raw)))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
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
    fn caller_resolves_implicit_sheet_and_rejects_an_unresolved_identifier() {
        use crate::registration::host::test_support::{CallbackScript, Reply};
        let mut cells = XLMREF12 {
            count: 1,
            reftbl: [XLREF12 {
                rw_first: 4,
                rw_last: 4,
                col_first: 5,
                col_last: 5,
            }],
        };
        let caller = XLOPER12 {
            value: XLOPER12Value {
                mref: XLOPER12MRef {
                    references: &mut cells,
                    sheet_id: 0,
                },
            },
            xltype: xlfn_sys::XLTYPE_REF,
        };
        for id in [42, 0] {
            let mut sheet = crate::utf16::encode_counted("[Book]Data", "test", 32767).unwrap();
            let name = XLOPER12 {
                value: XLOPER12Value {
                    string: sheet.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            };
            let resolved = XLOPER12 {
                value: XLOPER12Value {
                    mref: XLOPER12MRef {
                        references: std::ptr::null_mut(),
                        sheet_id: id,
                    },
                },
                xltype: xlfn_sys::XLTYPE_REF,
            };
            let script = CallbackScript::install([
                Reply::success(XLF_CALLER, caller),
                Reply::success(XL_SHEET_NM, name),
                Reply::success(XL_SHEET_ID, resolved),
            ]);
            let callbacks = HostCallbackSession::new();
            let result = ExcelHost::new(&callbacks).caller();
            if id == 0 {
                assert!(matches!(
                    (result).as_ref().map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::ExcelApi {
                        function: ExcelApiFunction::SheetId,
                        failure: ExcelApiFailure::UnexpectedResult,
                        ..
                    })
                ));
            } else {
                assert_eq!(
                    result.unwrap(),
                    HostCaller {
                        sheet_id: id,
                        row: 4,
                        column: 5
                    }
                );
            }
            script.assert_calls(&[XLF_CALLER, XL_SHEET_NM, XL_SHEET_ID]);
        }
    }

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
                    (caller_location(value))
                        .as_ref()
                        .map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::Input {
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
            (caller_location(value))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "caller",
                reason: InputError::Malformed("misaligned reference table pointer"),
            })
        ));
    }
}

#[cfg(test)]
mod coerce_tests {
    use super::*;
    use crate::error::InputError;
    use crate::reference::FromExcelReference;
    use crate::registration::host::test_support::{CallbackScript, Reply};
    use crate::value::raw::{CoercedGridRef, XlArrayRef};
    use std::cell::Cell;
    use xlfn_sys::{
        XLOPER12Array, XLOPER12SRef, XLOPER12Value, XLREF12, XLTYPE_MULTI, XLTYPE_SREF, XLTYPE_STR,
    };

    fn reference_raw() -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                sref: XLOPER12SRef {
                    count: 1,
                    reference: XLREF12 {
                        rw_first: 0,
                        rw_last: 0,
                        col_first: 0,
                        col_last: 1,
                    },
                },
            },
            xltype: XLTYPE_SREF,
        }
    }

    fn reference(raw: &XLOPER12) -> ExcelReference<'_> {
        ExcelReference::from_excel_reference(XlValueRef::from_array_cell(raw).unwrap(), "reference")
            .unwrap()
    }

    fn callback_text(units: &mut [u16]) -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                string: units.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        }
    }

    #[test]
    fn coerce_with_traverses_callback_array_without_an_owned_matrix() {
        let mut cells = [XLOPER12::number(1.5), XLOPER12::number(2.5)];
        let result = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 2,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let script = CallbackScript::install([Reply::success(XL_COERCE, result)]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        let visited = Cell::new(false);

        let sum = host
            .coerce_with(&reference, |value| {
                visited.set(true);
                let array = XlArrayRef::from_excel(value, "reference")?;
                assert_eq!(
                    array.shape(),
                    crate::value::Shape {
                        rows: 1,
                        columns: 2
                    }
                );
                array
                    .cells()
                    .try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))
            })
            .unwrap();
        assert!(visited.get());
        assert_eq!(sum, 4.0);
        script.assert_calls(&[XL_COERCE]);
    }

    #[test]
    fn miri_coerce_grid_checks_only_visited_cells_and_preserves_cleanup_priority() {
        use xlfn_sys::XLRET_FAILED;
        fn sum(value: XlValueRef<'_>, count: usize) -> XllResult<f64> {
            let grid = CoercedGridRef::new(value)?;
            assert_eq!(
                grid.shape(),
                crate::value::Shape {
                    rows: 1,
                    columns: 3
                }
            );
            assert_eq!(grid.len(), 3);
            grid.try_cells()
                .take(count)
                .try_fold(0.0, |sum, cell| Ok(sum + cell?.as_f64()?))
        }
        let mut cells = [
            XLOPER12::number(1.5),
            XLOPER12::integer(2),
            XLOPER12 {
                xltype: 0,
                ..XLOPER12::missing()
            },
        ];
        let result = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 3,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let script = CallbackScript::install([
            Reply::success(XL_COERCE, result),
            Reply::success(XL_COERCE, result).release_status(XLRET_FAILED),
            Reply::success(XL_COERCE, result).release_status(XLRET_FAILED),
            Reply::success(XL_COERCE, result),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        assert_eq!(
            host.coerce_with(&reference, |value| sum(value, 2)).unwrap(),
            3.5
        );
        assert!(matches!(
            (host.coerce_with(&reference, |value| sum(value, 3)))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "reference",
                reason: InputError::Malformed("unknown base xltype")
            })
        ));
        assert!(matches!(
            (host.coerce_with(&reference, |value| sum(value, 2)))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
                function: ExcelApiFunction::Free,
                ..
            })
        ));
        assert!(matches!(
            (host.coerce_with(&reference, |value| XlArrayRef::from_excel(
                value,
                "reference"
            )
            .map(|_| ())))
            .as_ref()
            .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "reference",
                reason: InputError::Malformed("unknown base xltype")
            })
        ));
        script.assert_calls(&[XL_COERCE; 4]);
    }

    #[test]
    fn coerce_grid_accepts_a_scalar_and_rejects_invalid_array_storage_before_iteration() {
        let number = XLOPER12::number(9.5);
        let grid = CoercedGridRef::new(XlValueRef::from_array_cell(&number).unwrap()).unwrap();
        assert_eq!(
            grid.shape(),
            crate::value::Shape {
                rows: 1,
                columns: 1
            }
        );
        assert!(!grid.is_empty());
        assert_eq!(
            grid.try_cells().next().unwrap().unwrap().as_f64().unwrap(),
            9.5
        );

        let missing_buffer = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 1,
                    values: std::ptr::null_mut(),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert!(matches!(
            (CoercedGridRef::new(XlValueRef::from_array_cell(&missing_buffer).unwrap()))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Input {
                argument: "reference",
                reason: InputError::NullPointer
            })
        ));
    }

    #[test]
    fn coerce_with_keeps_outer_text_live_during_nested_host_callback() {
        let mut outer = crate::utf16::encode_counted("外側🦀", "test", 32767).unwrap();
        let mut inner = crate::utf16::encode_counted("Inner", "test", 32767).unwrap();
        let script = CallbackScript::install([
            Reply::success(XL_COERCE, callback_text(&mut outer)),
            Reply::success(XL_SHEET_NM, callback_text(&mut inner)),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);

        let n = host
            .coerce_with(&reference, |value| {
                let text = value.as_str()?.as_utf16();
                assert!(text.iter().copied().eq("外側🦀".encode_utf16()));
                let nested = host.sheet_name(&reference)?;
                assert_eq!(nested, "Inner");
                // The inner callback and its xlFree must not invalidate the
                // borrowed result of the outer xlCoerce callback.
                assert!(text.iter().copied().eq("外側🦀".encode_utf16()));
                Ok(text.len())
            })
            .unwrap();
        assert_eq!(n, 4);
        script.assert_calls(&[XL_COERCE, XL_SHEET_NM]);
    }

    #[test]
    fn coerce_with_preserves_decode_error_over_cleanup_failure() {
        let script =
            CallbackScript::install([Reply::success(XL_COERCE, XLOPER12::number(5.0))
                .release_status(xlfn_sys::XLRET_FAILED)]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        let result: XllResult<()> = host.coerce_with(&reference, |value| {
            assert_eq!(value.as_f64()?, 5.0);
            Err(XllError::CLOSING)
        });
        assert!(matches!(result, Err(__err) if __err.is_closing()));
        script.assert_calls(&[XL_COERCE]);
    }

    #[test]
    fn coerce_with_reports_cleanup_failure_after_successful_visit() {
        let script =
            CallbackScript::install([Reply::success(XL_COERCE, XLOPER12::number(5.0))
                .release_status(xlfn_sys::XLRET_FAILED)]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        assert!(matches!(
            (host.coerce_with(&reference, |value| value.as_f64()))
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
                function: ExcelApiFunction::Free,
                ..
            })
        ));
        script.assert_calls(&[XL_COERCE]);
    }

    #[test]
    fn coerce_with_skips_visitor_on_terminal_callback() {
        let script = CallbackScript::install([
            Reply::success(XL_COERCE, XLOPER12::number(5.0)).status(xlfn_sys::XLRET_ABORT)
        ]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        let result: XllResult<()> = host.coerce_with(&reference, |_| {
            panic!("terminal callback must not expose a result")
        });
        assert!(matches!(
            (result).as_ref().map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::ExcelApi {
                function: ExcelApiFunction::Coerce,
                ..
            })
        ));
        script.assert_calls(&[XL_COERCE]);
    }

    #[test]
    fn coerce_with_nested_abort_suppresses_later_calls_but_frees_outer_result() {
        let mut inner = crate::utf16::encode_counted("Inner", "test", 32767).unwrap();
        let script = CallbackScript::install([
            Reply::success(XL_COERCE, XLOPER12::number(12.0)),
            Reply::success(XL_SHEET_NM, callback_text(&mut inner)).status(xlfn_sys::XLRET_UNCALCED),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        let value = host
            .coerce_with(&reference, |outer| {
                assert_eq!(outer.as_f64()?, 12.0);
                assert!(host.sheet_name(&reference).is_err());
                assert!(matches!(
                    (host.sheet_name(&reference))
                        .as_ref()
                        .map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::ExcelApi {
                        failure: ExcelApiFailure::Suppressed(ExcelCallbackStatus::Uncalced),
                        ..
                    })
                ));
                outer.as_f64()
            })
            .unwrap();
        assert_eq!(value, 12.0);
        assert_eq!(host.terminal_status(), Some(ExcelCallbackStatus::Uncalced));
        script.assert_calls(&[XL_COERCE, XL_SHEET_NM]);
    }

    #[test]
    fn coerce_with_panic_drops_callback_result_once() {
        let script = CallbackScript::install([Reply::success(XL_COERCE, XLOPER12::number(5.0))]);
        let callbacks = HostCallbackSession::new();
        let host = ExcelHost::new(&callbacks);
        let reference_raw = reference_raw();
        let reference = reference(&reference_raw);
        let outcome = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            let _: XllResult<()> = host.coerce_with(&reference, |_| panic!("visitor panic"));
        }));
        assert!(outcome.is_err());
        script.assert_calls(&[XL_COERCE]);
    }
}
