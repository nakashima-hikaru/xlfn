use std::fmt;

/// Compact diagnostic identifier used by internal failures and emitted events.
///
/// Named failure-site codes are eight-byte ASCII mnemonics packed in
/// big-endian order. Event sequence identifiers may use other numeric values;
/// neither form is a persisted protocol or a user-defined error namespace.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DiagnosticId(u64);

impl DiagnosticId {
    /// Returns the numeric representation used by logs and diagnostic JSON.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Raw diagnostic encoding for framework producers, not the facade API.
#[doc(hidden)]
pub mod internal {
    use super::DiagnosticId;

    pub const fn from_ascii8(bytes: [u8; 8]) -> DiagnosticId {
        DiagnosticId(u64::from_be_bytes(bytes))
    }
    pub const fn from_u64(value: u64) -> DiagnosticId {
        DiagnosticId(value)
    }
    pub const fn with_low_u32(id: DiagnosticId, value: u32) -> DiagnosticId {
        DiagnosticId((id.0 & 0xffff_ffff_0000_0000) | value as u64)
    }
}

impl fmt::LowerHex for DiagnosticId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ascii_codes_keep_their_numeric_and_hex_representation() {
        let id = super::internal::from_ascii8(*b"HANDSCOP");

        assert_eq!(id.as_u64(), 0x4841_4e44_5343_4f50);
        assert_eq!(format!("{id:016x}"), "48414e4453434f50");
    }
}
