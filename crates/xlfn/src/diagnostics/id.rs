use std::fmt;

/// Compact diagnostic identifier used by internal failures and emitted events.
///
/// Named failure-site codes are eight-byte ASCII mnemonics packed in
/// big-endian order. Event sequence identifiers may use other numeric values;
/// neither form is a persisted protocol or a user-defined error namespace.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiagnosticId(u64);

// Some codes are only referenced by platform- or feature-gated paths.
#[allow(
    dead_code,
    reason = "some diagnostic codes are only used by platform- or feature-gated paths"
)]
impl DiagnosticId {
    /// Creates a diagnostic code from an eight-byte ASCII mnemonic.
    pub(crate) const fn from_ascii8(bytes: [u8; 8]) -> Self {
        Self(u64::from_be_bytes(bytes))
    }

    /// Returns the numeric representation used by logs and diagnostic JSON.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn with_low_u32(self, value: u32) -> Self {
        Self((self.0 & 0xffff_ffff_0000_0000) | value as u64)
    }
}

impl fmt::LowerHex for DiagnosticId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::DiagnosticId;

    #[test]
    fn ascii_codes_keep_their_numeric_and_hex_representation() {
        let id = DiagnosticId::from_ascii8(*b"HANDSCOP");

        assert_eq!(id.as_u64(), 0x4841_4e44_5343_4f50);
        assert_eq!(format!("{id:016x}"), "48414e4453434f50");
    }
}
