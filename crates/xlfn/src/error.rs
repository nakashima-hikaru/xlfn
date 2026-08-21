use std::fmt;
use std::path::PathBuf;

pub type XllResult<T> = Result<T, XllError>;

/// Converts an application-local error at the single Excel boundary.
pub trait IntoXllError {
    fn into_xll_error(self) -> XllError;
}

impl IntoXllError for XllError {
    fn into_xll_error(self) -> XllError {
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum ExcelError {
    Null = xlfn_sys::XLERR_NULL,
    DivisionByZero = xlfn_sys::XLERR_DIV0,
    Value = xlfn_sys::XLERR_VALUE,
    Reference = xlfn_sys::XLERR_REF,
    Name = xlfn_sys::XLERR_NAME,
    Number = xlfn_sys::XLERR_NUM,
    NotAvailable = xlfn_sys::XLERR_NA,
    GettingData = xlfn_sys::XLERR_GETTING_DATA,
}

impl ExcelError {
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        match code {
            xlfn_sys::XLERR_NULL => Some(Self::Null),
            xlfn_sys::XLERR_DIV0 => Some(Self::DivisionByZero),
            xlfn_sys::XLERR_VALUE => Some(Self::Value),
            xlfn_sys::XLERR_REF => Some(Self::Reference),
            xlfn_sys::XLERR_NAME => Some(Self::Name),
            xlfn_sys::XLERR_NUM => Some(Self::Number),
            xlfn_sys::XLERR_NA => Some(Self::NotAvailable),
            xlfn_sys::XLERR_GETTING_DATA => Some(Self::GettingData),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputError {
    NullPointer,
    WrongType { expected: &'static str, actual: u32 },
    NonFinite,
    NotInteger,
    NumericOverflow,
    OutOfRange,
    InvalidUtf16,
    Malformed(&'static str),
    TooLarge { limit: usize, actual: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shape {
    pub rows: usize,
    pub columns: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainErrorCode {
    InvalidInput,
    Overflow,
    NativeFailure,
}

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

    pub(crate) const ASYNC_SPAWN: Self = Self::from_ascii8(*b"ASYNCSPN");
    pub(crate) const ASYNC_TIME: Self = Self::from_ascii8(*b"ASYNTIME");
    pub(crate) const CACHE_REENTRANT: Self = Self::from_ascii8(*b"CACHEREC");
    pub(crate) const CACHE_TYPE: Self = Self::from_ascii8(*b"CACHETYP");
    pub(crate) const DIAGNOSTICS_CLOSE: Self = Self::from_ascii8(*b"DIAGCLOS");
    pub(crate) const DIAGNOSTICS_FAILURE: Self = Self::from_ascii8(*b"DIAGFAIL");
    pub(crate) const DIAGNOSTICS_PENDING: Self = Self::from_ascii8(*b"DIAGPEND");
    pub(crate) const DIAGNOSTICS_RESET: Self = Self::from_ascii8(*b"DIAGRSET");
    pub(crate) const HANDLE_SLOT: Self = Self::from_ascii8(*b"HANDSLOT");
    pub(crate) const HANDLE_ENTROPY: Self = Self::from_ascii8(*b"HANDRNGF");
    pub(crate) const HANDLE_TOPIC_COLLISION: Self = Self::from_ascii8(*b"HANDRTDC");
    pub(crate) const ASYNC_FEATURE: Self = Self::from_ascii8(*b"ASYNFEAT");
    pub(crate) const FAILURE: Self = Self::from_ascii8(*b"\0\0\0\0FAIL");
    pub(crate) const LEAN_TRACE: Self = Self::from_ascii8(*b"LEANTRCE");
    pub(crate) const OPEN_STATE: Self = Self::from_ascii8(*b"OPENSTAT");
    pub(crate) const OPEN_ROLLBACK_FAILURE: Self = Self::from_ascii8(*b"OPRBFAIL");
    pub(crate) const OPEN_ROLLBACK_PENDING: Self = Self::from_ascii8(*b"OPRBPEND");
    pub(crate) const QUIESCENCE_FAILURE: Self = Self::from_ascii8(*b"QUIESCEF");
    pub(crate) const REGISTRATION_UNKNOWN: Self = Self::from_ascii8(*b"REGSUNKN");
    pub(crate) const RTD_GIT_QUIESCENCE: Self = Self::from_ascii8(*b"RTD_GITQ");
    pub(crate) const STATE_SCAN: Self = Self::from_ascii8(*b"STATESCA");
    pub(crate) const TEST_RETRY: Self = Self::from_ascii8(*b"TESTRTRY");
    pub(crate) const REGISTRY: Self = Self::from_ascii8(*b"REGISTRY");
    pub(crate) const REGISTRATION_SIGNATURE: Self = Self::from_ascii8(*b"REGSIGNA");
    pub(crate) const STRING_LENGTH: Self = Self::from_ascii8(*b"STRINGLE");
    pub(crate) const HANDLE_CALLBACKS: Self = Self::from_ascii8(*b"HANDCBKS");
    pub(crate) const HANDLE_CONTEXT: Self = Self::from_ascii8(*b"HANDCTXT");
    pub(crate) const HANDLE_PINS: Self = Self::from_ascii8(*b"HANDPINS");
    pub(crate) const HANDLE_DIGEST: Self = Self::from_ascii8(*b"HANDDIGE");
    pub(crate) const HANDLE_UDF: Self = Self::from_ascii8(*b"HANDUDFI");
    pub(crate) const RETURN_REOPEN: Self = Self::from_ascii8(*b"RTNREOPN");
    pub(crate) const RTD_HANDLE: Self = Self::from_ascii8(*b"RTDHANDL");
    pub(crate) const RTD_MULTI: Self = Self::from_ascii8(*b"RTDMULTI");
    pub(crate) const RTD_DISPATCH: Self = Self::from_ascii8(*b"RTDDISPT");
    pub(crate) const GIT_NULL: Self = Self::from_ascii8(*b"GIT_NULL");
    pub(crate) const ATTEMPT_OVERFLOW: Self = Self::from_ascii8(*b"ATTMOVFL");
    pub(crate) const ATTEMPT_ZERO: Self = Self::from_ascii8(*b"ATTMZERO");
    pub(crate) const CLOSE_LEASE_GATE: Self = Self::from_ascii8(*b"CLLOSEGE");
    pub(crate) const CLOSE_CERTIFICATE: Self = Self::from_ascii8(*b"CLOSECER");
    pub(crate) const CLOSE_RUNTIME: Self = Self::from_ascii8(*b"CLOSERUN");
    pub(crate) const CLOSE_GHOST: Self = Self::from_ascii8(*b"CLOSTGHO");
    pub(crate) const CLOSE_RTD_SUBSCRIPTION: Self = Self::from_ascii8(*b"CLOSTRSU");
    pub(crate) const CLOSE_WAIT: Self = Self::from_ascii8(*b"CLOSWTNO");
    pub(crate) const GHOST_GENERATION: Self = Self::from_ascii8(*b"GHOSTGEN");
    pub(crate) const MISSING_STATE: Self = Self::from_ascii8(*b"MISSSTAT");
    pub(crate) const OPEN_PHASE: Self = Self::from_ascii8(*b"OPENPHAS");
    pub(crate) const MODULE_RESIDENCY: Self = Self::from_ascii8(*b"MODRESID");
    pub(crate) const OPEN_ROLLBACK_CERTIFICATE: Self = Self::from_ascii8(*b"OPRBCERT");
    pub(crate) const OPEN_ROLLBACK_CERT_UNKNOWN: Self = Self::from_ascii8(*b"OPRBCERU");
    pub(crate) const OPEN_ROLLBACK_PHASE: Self = Self::from_ascii8(*b"OPRBPHAS");
    pub(crate) const RTD_SUBSCRIPTION_OVERFLOW: Self = Self::from_ascii8(*b"RTDSUBOV");
    pub(crate) const RTD_SLOTS: Self = Self::from_ascii8(*b"RTDSLOTS");
    pub(crate) const TICKET_OVERFLOW: Self = Self::from_ascii8(*b"TICKOVFL");
    pub(crate) const RTD_INDEX_DUPLICATE: Self = Self::from_ascii8(*b"RTDIDXDU");
    pub(crate) const RTD_RT_ID_OVERFLOW: Self = Self::from_ascii8(*b"RTDRTIDO");
    pub(crate) const RTD_SUBSCRIPTION_ID_OVERFLOW: Self = Self::from_ascii8(*b"RTDSIDOV");
    pub(crate) const ACTIVE_KEY_DUPLICATE: Self = Self::from_ascii8(*b"ACTVKEYD");
    pub(crate) const CONNECTION_INFLIGHT: Self = Self::from_ascii8(*b"CONNINFL");
    pub(crate) const PANIC_SOURCE: Self = Self::from_ascii8(*b"PANICSRC");
    pub(crate) const PANIC_SUBSCRIPTION: Self = Self::from_ascii8(*b"PANICSUB");
    pub(crate) const RESERVATION_OVERFLOW: Self = Self::from_ascii8(*b"RESVOVFL");
    pub(crate) const RTD_INDEX_ORPHAN: Self = Self::from_ascii8(*b"RTDIDXOR");
    pub(crate) const RTD_SERVER_DUE: Self = Self::from_ascii8(*b"RTDSRVDU");
    pub(crate) const SERVER_GENERATION_MISMATCH: Self = Self::from_ascii8(*b"SRVGENMI");
    pub(crate) const NO_REFERENCE: Self = Self::from_ascii8(*b"NOREFRAC");
    pub(crate) const OVERLAPPED_REFERENCE: Self = Self::from_ascii8(*b"OVLPREFR");
    pub(crate) const PANIC_DISCONNECT: Self = Self::from_ascii8(*b"PANICDIS");
    pub(crate) const PANIC_NOTIFY: Self = Self::from_ascii8(*b"PANICNOT");
    pub(crate) const REFERENCE_OVERFLOW: Self = Self::from_ascii8(*b"REFOVFLW");
    pub(crate) const REFERENCE_ID_MISMATCH: Self = Self::from_ascii8(*b"REFRIDMI");
    pub(crate) const TOPIC_ID_DUPLICATE: Self = Self::from_ascii8(*b"TOPICIDD");
    pub(crate) const TOPIC_KEY_DUPLICATE: Self = Self::from_ascii8(*b"TOPICKEY");
    pub(crate) const GRID_INDEX: Self = Self::from_ascii8(*b"GRIDINDX");
    pub(crate) const HANDLE_NO_CONTEXT: Self = Self::from_ascii8(*b"HANDNOCT");
    pub(crate) const HANDLE_SCOPE_MISSING: Self = Self::from_ascii8(*b"HANDSCOP");
    pub(crate) const RTD_WINDOW_STATUS: Self = Self::from_ascii8(*b"RTDW\0\0\0\0");
    pub(crate) const RTD_WINDOW_FAILURE: Self = Self::from_ascii8(*b"RTDWFAIL");
    pub(crate) const RTD_SERVER_GENERATION_EXHAUSTED: Self = Self::from_ascii8(*b"SRVGENEX");
    pub(crate) const TEST_SENTINEL: Self = Self::from_u64(0xDEAD_BEEF);
}

impl fmt::LowerHex for DiagnosticId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum XllError {
    #[error("invalid argument {argument}: {reason:?}")]
    Input {
        argument: &'static str,
        reason: InputError,
    },
    #[error(
        "shape mismatch: expected {}x{}, got {}x{}",
        .expected.rows,
        .expected.columns,
        .actual.rows,
        .actual.columns
    )]
    Shape { expected: Shape, actual: Shape },
    #[error(
        "element count mismatch for {rows}x{columns} matrix: expected {expected}, got {actual}"
    )]
    ElementCountMismatch {
        rows: usize,
        columns: usize,
        expected: usize,
        actual: usize,
    },
    #[error("domain error: {code:?}")]
    Domain { code: DomainErrorCode },
    #[error("Excel API {function} failed with {code}")]
    ExcelApi { function: &'static str, code: i32 },
    #[error("Excel name {name} is already registered")]
    RegistrationConflict { name: &'static str },
    #[error("Excel name {name} no longer refers to the expected registration")]
    MetadataDebtBindingChanged { name: &'static str },
    #[error("failed to load {} (OS error {os_error})", path.display())]
    LibraryLoad { path: PathBuf, os_error: u32 },
    #[error("missing symbol {symbol}")]
    MissingSymbol { symbol: &'static str },
    #[error("ABI mismatch: expected {expected}, got {actual}")]
    AbiMismatch { expected: u32, actual: u32 },
    #[error("native error {code}: {message}")]
    Native { code: i32, message: String },
    #[error(
        "RTD subscription shutdown failed for server generation {server_generation}, topic {topic_id}, key {key}: {source}"
    )]
    RtdSubscriptionShutdown {
        server_generation: u64,
        topic_id: i32,
        key: String,
        #[source]
        source: Box<XllError>,
    },
    #[error("Excel error value {0:?}")]
    ExcelValue(ExcelError),
    #[error("invalid handle")]
    InvalidHandle,
    #[error("stale handle")]
    StaleHandle,
    #[error("add-in is closing")]
    Closing,
    #[error("runtime capacity is exhausted")]
    Overloaded,
    #[error("reentrant call would wait for itself")]
    ReentrantCall,
    #[error("panic was caught at the XLL boundary")]
    Panic,
    #[error("internal error (diagnostic {diagnostic_id:016x})")]
    Internal { diagnostic_id: DiagnosticId },
}

impl XllError {
    #[must_use]
    pub const fn input(argument: &'static str, reason: InputError) -> Self {
        Self::Input { argument, reason }
    }

    #[must_use]
    pub const fn excel_error(&self) -> ExcelError {
        match self {
            Self::Domain { .. } => ExcelError::Number,
            Self::Input {
                reason: InputError::NumericOverflow,
                ..
            } => ExcelError::Number,
            Self::InvalidHandle
            | Self::StaleHandle
            | Self::Closing
            | Self::Overloaded
            | Self::ReentrantCall => ExcelError::NotAvailable,
            Self::ExcelValue(error) => *error,
            Self::Input { .. }
            | Self::Shape { .. }
            | Self::ElementCountMismatch { .. }
            | Self::ExcelApi { .. }
            | Self::RegistrationConflict { .. }
            | Self::MetadataDebtBindingChanged { .. }
            | Self::LibraryLoad { .. }
            | Self::MissingSymbol { .. }
            | Self::AbiMismatch { .. }
            | Self::Native { .. }
            | Self::RtdSubscriptionShutdown { .. }
            | Self::Panic
            | Self::Internal { .. } => ExcelError::Value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_documented_excel_values() {
        assert_eq!(
            XllError::input("x", InputError::NonFinite).excel_error(),
            ExcelError::Value
        );
        assert_eq!(
            XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: 1,
                },
                actual: Shape {
                    rows: 2,
                    columns: 1,
                },
            }
            .excel_error(),
            ExcelError::Value
        );
        assert_eq!(
            XllError::Domain {
                code: DomainErrorCode::Overflow,
            }
            .excel_error(),
            ExcelError::Number
        );
        assert_eq!(
            XllError::input("x", InputError::NumericOverflow).excel_error(),
            ExcelError::Number
        );
        assert_eq!(
            XllError::StaleHandle.excel_error(),
            ExcelError::NotAvailable
        );
    }

    #[test]
    fn rtd_shutdown_error_preserves_owner_and_source_context() {
        let error = XllError::RtdSubscriptionShutdown {
            server_generation: 7,
            topic_id: 42,
            key: "stream:test".to_owned(),
            source: Box::new(XllError::Panic),
        };
        let message = error.to_string();
        assert!(message.contains("server generation 7"));
        assert!(message.contains("topic 42"));
        assert!(message.contains("stream:test"));
        assert!(message.contains("panic was caught"));
    }

    #[test]
    fn diagnostic_id_preserves_ascii_code_and_hex_display() {
        let id = DiagnosticId::from_ascii8(*b"HANDSCOP");

        assert_eq!(id.as_u64(), 0x4841_4e44_5343_4f50);
        assert_eq!(format!("{id:016x}"), "48414e4453434f50");
    }
}
