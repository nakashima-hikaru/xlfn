use crate::XllResult;
use crate::host_api::ExcelHost;
use crate::input_identity::InputFingerprint;
use core::fmt::NumBuffer;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FormulaCaller {
    pub(crate) sheet_id: xlfn_sys::IDSHEET,
    pub(crate) row: i32,
    pub(crate) column: i32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FormulaRevisionKey {
    pub(crate) caller: FormulaCaller,
    /// Scopes the semantic input encoding to one fixed Rust parameter schema.
    pub(crate) udf_id: &'static str,
    pub(crate) inputs: InputFingerprint,
}

impl FormulaRevisionKey {
    pub(crate) fn new(
        caller: FormulaCaller,
        udf_id: &'static str,
        inputs: InputFingerprint,
    ) -> Self {
        Self {
            caller,
            udf_id,
            inputs,
        }
    }

    /// Serialize the structured identity at the Excel RTD boundary.
    ///
    /// This representation is part of the Excel protocol and must remain
    /// byte-for-byte compatible with the previous formula topic formatter.
    pub(crate) fn format_rtd_key(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        // 20 digits for a 64-bit IDSHEET, 11 for each i32 coordinate, four
        // separators, and the 64-character digest. This upper bound keeps the
        // complete key in one allocation on both supported pointer widths.
        const NUMERIC_KEY_CAPACITY: usize = 20 + 11 + 11 + 4;
        let mut result = String::with_capacity(
            NUMERIC_KEY_CAPACITY + self.udf_id.len() + self.inputs.as_bytes().len() * 2,
        );

        let mut sheet_buffer = NumBuffer::<usize>::new();
        let mut coordinate_buffer = NumBuffer::<i32>::new();
        result.push_str(self.caller.sheet_id.format_into(&mut sheet_buffer));
        result.push('\x1f');
        result.push_str(self.caller.row.format_into(&mut coordinate_buffer));
        result.push('\x1f');
        result.push_str(self.caller.column.format_into(&mut coordinate_buffer));
        result.push('\x1f');
        result.push_str(self.udf_id);
        result.push('\x1f');

        for byte in self.inputs.as_bytes() {
            result.push(HEX[(byte >> 4) as usize] as char);
            result.push(HEX[(byte & 0x0f) as usize] as char);
        }

        result
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HandleTopicKey {
    Formula(FormulaRevisionKey),
}

impl HandleTopicKey {
    pub(crate) fn format_rtd_key(&self) -> String {
        match self {
            Self::Formula(key) => key.format_rtd_key(),
        }
    }
}

#[cfg(test)]
pub(crate) fn test_topic_key(label: &str) -> HandleTopicKey {
    let inputs = InputFingerprint::from_bytes(*blake3::hash(label.as_bytes()).as_bytes());
    HandleTopicKey::Formula(FormulaRevisionKey::new(
        FormulaCaller {
            sheet_id: 0,
            row: 0,
            column: 0,
        },
        "TEST.HANDLE",
        inputs,
    ))
}

pub(crate) fn resolve_formula_caller(
    callbacks: &crate::host_callback::HostCallbackSession,
) -> XllResult<FormulaCaller> {
    let caller = ExcelHost::new(callbacks).caller()?;
    Ok(FormulaCaller {
        sheet_id: caller.sheet_id,
        row: caller.row,
        column: caller.column,
    })
}

pub(crate) fn formula_revision_key(
    callbacks: &crate::host_callback::HostCallbackSession,
    udf_id: &'static str,
    inputs: InputFingerprint,
) -> XllResult<HandleTopicKey> {
    let caller = resolve_formula_caller(callbacks)?;
    Ok(HandleTopicKey::Formula(FormulaRevisionKey::new(
        caller, udf_id, inputs,
    )))
}
