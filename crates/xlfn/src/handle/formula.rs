use crate::XllResult;
use crate::host_api::ExcelHost;
use crate::input_identity::InputFingerprint;
use core::fmt::NumBuffer;
use std::sync::atomic::{AtomicU64, Ordering};

// Shared by every producer in this loaded framework instance. Exhaustion is
// terminal for fresh revisions; the counter must never wrap and reuse an ID.
static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn fresh_invocation_inputs(inputs: InputFingerprint) -> XllResult<InputFingerprint> {
    fresh_inputs_with_counter(inputs, &NEXT_INVOCATION)
}

fn fresh_inputs_with_counter(
    inputs: InputFingerprint,
    counter: &AtomicU64,
) -> XllResult<InputFingerprint> {
    let nonce = counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| {
            crate::XllError::custom(
                crate::ExcelError::Number,
                "handle invocation identity exhausted",
            )
        })?;
    // Retain the existing collision-resistant topic protocol, with a separate
    // domain for fresh invocations so it cannot alias an input-only revision.
    let mut identity = blake3::Hasher::new_derive_key("xlfn.handle-invocation.v1");
    identity.update(inputs.as_bytes());
    identity.update(&nonce.to_le_bytes());
    Ok(InputFingerprint::from_bytes(
        *identity.finalize().as_bytes(),
    ))
}

#[cfg(test)]
mod invocation_tests {
    use super::*;

    #[test]
    fn fresh_identity_never_wraps_or_reuses_the_input_identity() {
        let inputs = InputFingerprint::from_bytes([7; 32]);
        let counter = AtomicU64::new(u64::MAX - 1);
        let first = fresh_inputs_with_counter(inputs, &counter).unwrap();
        assert_ne!(first, inputs);
        assert!(fresh_inputs_with_counter(inputs, &counter).is_err());
        assert!(fresh_inputs_with_counter(inputs, &counter).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}

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

    /// Serialize the structured identity for the private Excel lifetime
    /// observer boundary.
    ///
    /// This representation is part of the Excel protocol and must remain
    /// byte-for-byte compatible with the previous formula topic formatter.
    pub(crate) fn format_lifetime_key(&self) -> String {
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
    pub(crate) fn format_lifetime_key(&self) -> String {
        match self {
            Self::Formula(key) => key.format_lifetime_key(),
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

pub(crate) fn resolve_formula_caller(host: ExcelHost<'_>) -> XllResult<FormulaCaller> {
    let caller = host.caller()?;
    Ok(FormulaCaller {
        sheet_id: caller.sheet_id,
        row: caller.row,
        column: caller.column,
    })
}

pub(crate) fn formula_revision_key(
    host: ExcelHost<'_>,
    udf_id: &'static str,
    inputs: InputFingerprint,
) -> XllResult<HandleTopicKey> {
    let caller = resolve_formula_caller(host)?;
    Ok(HandleTopicKey::Formula(FormulaRevisionKey::new(
        caller, udf_id, inputs,
    )))
}
