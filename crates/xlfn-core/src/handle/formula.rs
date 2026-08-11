use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormulaCaller {
    pub(crate) sheet_id: xlfn_sys::IDSHEET,
    pub(crate) row: i32,
    pub(crate) column: i32,
}

pub(crate) fn format_formula_topic_key(
    caller: FormulaCaller,
    udf_id: &'static str,
    argument_digest: &[u8; 32],
) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // 20 digits for a 64-bit IDSHEET, 11 for each i32 coordinate, four
    // separators, and the 64-character digest. This upper bound keeps the
    // complete key in one allocation on both supported pointer widths.
    const NUMERIC_KEY_CAPACITY: usize = 20 + 11 + 11 + 4;
    let mut result =
        String::with_capacity(NUMERIC_KEY_CAPACITY + udf_id.len() + argument_digest.len() * 2);

    write!(
        &mut result,
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}",
        caller.sheet_id, caller.row, caller.column, udf_id,
    )
    .expect("writing to String cannot fail");

    for byte in argument_digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }

    result
}

pub(crate) fn resolve_formula_caller(
    callbacks: &crate::host_callback::HostCallbackSession,
) -> XllResult<FormulaCaller> {
    use xlfn_sys::{XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLTYPE_REF, XLTYPE_SREF};

    // SAFETY: this runs synchronously on the generated main-thread UDF boundary.
    let (status, mut caller) = unsafe {
        callbacks
            .call(XLF_CALLER, &[])
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlfCaller(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if status != ExcelCallbackStatus::Success {
        return Err(caller.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlfCaller",
            code: status.raw_code(),
        }));
    }
    let (row, column, sheet_id) = {
        let value = caller.borrow()?;
        match value.base_type() {
            XLTYPE_SREF => {
                // SAFETY: the type selects the SRef member.
                let reference = unsafe { value.raw().value.sref };
                if reference.count != 1
                    || reference.reference.rw_first != reference.reference.rw_last
                    || reference.reference.col_first != reference.reference.col_last
                {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                (
                    reference.reference.rw_first,
                    reference.reference.col_first,
                    None,
                )
            }
            XLTYPE_REF => {
                // SAFETY: the type selects the MRef member.
                let reference = unsafe { value.raw().value.mref };
                // SAFETY: Excel supplies a readable reference table.
                let table = unsafe { reference.references.as_ref() }
                    .ok_or_else(|| XllError::input("caller", crate::InputError::NullPointer))?;
                if table.count != 1 {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                let area = table.reftbl[0];
                if area.rw_first != area.rw_last || area.col_first != area.col_last {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                (area.rw_first, area.col_first, Some(reference.sheet_id))
            }
            _ => {
                return Err(XllError::input(
                    "caller",
                    crate::InputError::Malformed(
                        "handle-producing functions require a worksheet caller",
                    ),
                ));
            }
        }
    };

    if let Some(sheet_id) = sheet_id {
        caller.try_release()?;
        return Ok(FormulaCaller {
            sheet_id,
            row,
            column,
        });
    }

    let caller_arguments = [caller.raw_pointer()?];
    // SAFETY: caller remains live for the nested xlSheetNm callback.
    let (sheet_status, mut sheet) = unsafe {
        callbacks
            .call(XL_SHEET_NM, &caller_arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlSheetNm(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if sheet_status != ExcelCallbackStatus::Success {
        return Err(sheet.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlSheetNm",
            code: sheet_status.raw_code(),
        }));
    }
    // `xlSheetId` accepts the counted external sheet name returned by
    // `xlSheetNm`. The name is only a lookup input; it must never become part
    // of formula identity because workbook and worksheet names can change.
    let sheet_name_argument = [sheet.raw_pointer()?];
    // SAFETY: the counted sheet-name result remains live for this nested
    // callback and the callback session owns its release obligation.
    let (sheet_id_status, mut sheet_id_value) = unsafe {
        callbacks
            .call(XL_SHEET_ID, &sheet_name_argument)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlSheetId(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if sheet_id_status != ExcelCallbackStatus::Success {
        return Err(sheet_id_value
            .try_release()
            .err()
            .unwrap_or(XllError::ExcelApi {
                function: "xlSheetId",
                code: sheet_id_status.raw_code(),
            }));
    }
    let sheet_id = {
        let value = sheet_id_value.borrow()?;
        if value.base_type() != XLTYPE_REF {
            return Err(XllError::input(
                "caller",
                crate::InputError::Malformed("xlSheetId did not return an external reference"),
            ));
        }
        // SAFETY: XLTYPE_REF selects the MRef member, whose sheet_id is the
        // stable Excel worksheet identifier returned by xlSheetId.
        unsafe { value.raw().value.mref.sheet_id }
    };
    sheet_id_value.try_release()?;
    sheet.try_release()?;
    caller.try_release()?;

    Ok(FormulaCaller {
        sheet_id,
        row,
        column,
    })
}

pub(crate) fn formula_topic_key(
    callbacks: &crate::host_callback::HostCallbackSession,
    udf_id: &'static str,
    argument_digest: &[u8; 32],
) -> XllResult<String> {
    let caller = resolve_formula_caller(callbacks)?;
    Ok(format_formula_topic_key(caller, udf_id, argument_digest))
}
