//! Worksheet error values, boundary failures, and application error conversion.
//!
//! [`XllError`] keeps diagnostic detail separate from the [`ExcelError`] returned
//! to a worksheet. Applications can return `Result<T, ExcelError>` directly, use
//! [`XllError::custom`] for a chosen cell error with diagnostic text, or implement
//! [`IntoXllError`](crate::error::IntoXllError) for an application-specific error type.

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::diagnostics::id::DiagnosticId;
use crate::value::XlValueType;

/// A calculation or lifecycle result with framework diagnostic detail.
pub type XllResult<T> = Result<T, XllError>;

#[cfg(any(feature = "handles", feature = "rtd"))]
pub(crate) fn map_service_slot_error(
    error: xlfn_kernel::service_slot::ServiceSlotError<XllError>,
) -> XllError {
    match error {
        xlfn_kernel::service_slot::ServiceSlotError::Closed => XllError::Closing,
        xlfn_kernel::service_slot::ServiceSlotError::Fault(fault) => match fault {
            xlfn_kernel::service_slot::ServiceFault::Error(error) => error,
            xlfn_kernel::service_slot::ServiceFault::Panicked => XllError::Panic,
        },
    }
}

/// Converts an application-local error at the single Excel boundary.
pub trait IntoXllError {
    /// Converts this failure without performing Excel callbacks.
    fn into_xll_error(self) -> XllError;
}

impl IntoXllError for XllError {
    fn into_xll_error(self) -> XllError {
        self
    }
}

/// A worksheet error code supported by the Excel C API.
///
/// Unknown codes are rejected by [`Self::from_code`]; future supported codes may
/// be added without making exhaustive matches part of the public contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum ExcelError {
    /// `#NULL!`: an invalid intersection of references.
    Null = xlfn_sys::XLERR_NULL,
    /// `#DIV/0!`: division by zero.
    DivisionByZero = xlfn_sys::XLERR_DIV0,
    /// `#VALUE!`: an invalid value, type, or structure.
    Value = xlfn_sys::XLERR_VALUE,
    /// `#REF!`: an invalid cell reference.
    Reference = xlfn_sys::XLERR_REF,
    /// `#NAME?`: an unrecognized name.
    Name = xlfn_sys::XLERR_NAME,
    /// `#NUM!`: a numeric or domain failure.
    Number = xlfn_sys::XLERR_NUM,
    /// `#N/A`: a result is unavailable.
    NotAvailable = xlfn_sys::XLERR_NA,
    /// `#GETTING_DATA`: Excel is waiting for external data.
    GettingData = xlfn_sys::XLERR_GETTING_DATA,
}

impl ExcelError {
    /// Returns the corresponding Excel C API error code.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Decodes a supported Excel C API error code, returning `None` for unknown codes.
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

impl fmt::Display for ExcelError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::Null => "#NULL!",
            Self::DivisionByZero => "#DIV/0!",
            Self::Value => "#VALUE!",
            Self::Reference => "#REF!",
            Self::Name => "#NAME?",
            Self::Number => "#NUM!",
            Self::NotAvailable => "#N/A",
            Self::GettingData => "#GETTING_DATA",
        })
    }
}

impl std::error::Error for ExcelError {}

impl From<ExcelError> for XllError {
    fn from(error: ExcelError) -> Self {
        Self::ExcelValue(error)
    }
}

impl IntoXllError for ExcelError {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

impl From<io::Error> for XllError {
    fn from(error: io::Error) -> Self {
        Self::Native {
            code: error.raw_os_error().unwrap_or(0),
            message: error.to_string(),
        }
    }
}

impl IntoXllError for io::Error {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

impl From<crate::diagnostics::DiagnosticInitError> for XllError {
    fn from(error: crate::diagnostics::DiagnosticInitError) -> Self {
        use crate::diagnostics::DiagnosticInitError;
        match error {
            DiagnosticInitError::Io(error) => error.into(),
            error @ DiagnosticInitError::WorkerSpawn(_) => {
                Self::custom(ExcelError::Value, error.to_string())
            }
            DiagnosticInitError::ReentrantMutation => Self::ReentrantCall,
            DiagnosticInitError::RouterClosed => Self::Closing,
        }
    }
}

impl IntoXllError for crate::diagnostics::DiagnosticInitError {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

/// Why an Excel argument could not be decoded into the requested Rust type.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum InputError {
    /// Excel supplied a null argument pointer.
    #[error("null argument pointer")]
    NullPointer,
    /// The incoming Excel value has an incompatible type.
    #[error("expected {expected}, got {actual:?}")]
    WrongType {
        /// A human-readable description of the accepted type.
        expected: &'static str,
        /// The incoming Excel value's type.
        actual: XlValueType,
    },
    /// A numeric input is NaN or infinite.
    #[error("number is not finite")]
    NonFinite,
    /// A numeric input has a fractional component where an integer is required.
    #[error("number is not an integer")]
    NotInteger,
    /// A numeric input cannot be represented in the requested Rust type.
    #[error("number overflows the requested type")]
    NumericOverflow,
    /// An input is outside its allowed range.
    #[error("value is outside the allowed range")]
    OutOfRange,
    /// An Excel string contains unpaired UTF-16 surrogates.
    #[error("string is not valid UTF-16")]
    InvalidUtf16,
    /// An input violates an application or framework structural rule.
    #[error("malformed input: {0}")]
    Malformed(&'static str),
    /// An input exceeds a declared element or byte limit.
    #[error("input exceeds limit {limit}: got {actual}")]
    TooLarge {
        /// The maximum accepted size.
        limit: usize,
        /// The incoming size.
        actual: usize,
    },
}

/// The number of rows and columns in an Excel array or Rust matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Shape {
    /// The number of rows.
    pub rows: usize,
    /// The number of columns.
    pub columns: usize,
}

/// Stable, coarse categories for calculation failures.
///
/// Use [`XllError::custom`] when an application needs a diagnostic message and
/// its own worksheet error mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DomainErrorCode {
    /// The calculation's mathematical domain excludes the supplied input.
    InvalidInput,
    /// A calculation overflowed its representable numeric range.
    Overflow,
    /// An external calculation failed.
    NativeFailure,
}

/// Represents the terminal or recoverable status returned by Excel C API callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExcelCallbackStatus {
    /// Excel completed the callback successfully.
    Success,
    /// Excel aborted the callback or calculation.
    Abort,
    /// Excel could not satisfy a calculation dependency at this point.
    Uncalced,
    /// Another Excel callback return code.
    Failed(i32),
}

impl fmt::Display for ExcelCallbackStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "XLRET_SUCCESS"),
            Self::Abort => write!(f, "XLRET_ABORT"),
            Self::Uncalced => write!(f, "XLRET_UNCALCED"),
            Self::Failed(code) => write!(f, "{code}"),
        }
    }
}

impl ExcelCallbackStatus {
    pub(crate) fn from_raw(status: i32) -> Self {
        match status {
            xlfn_sys::XLRET_SUCCESS => Self::Success,
            xlfn_sys::XLRET_ABORT => Self::Abort,
            xlfn_sys::XLRET_UNCALCED => Self::Uncalced,
            other => Self::Failed(other),
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Abort | Self::Uncalced)
    }
}

/// An Excel C API operation identified in diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ExcelApiFunction {
    /// `xlfCaller`.
    Caller,
    /// `xlSheetNm`.
    SheetName,
    /// `xlSheetId`.
    SheetId,
    /// `xlGetName`.
    GetName,
    /// `xlfRegister`.
    Register,
    /// `xlfUnregister`.
    Unregister,
    /// `xlfSetName`.
    SetName,
    /// `xlfEvaluate`.
    Evaluate,
    /// `xlEventRegister`.
    EventRegister,
    /// `xlfRtd`.
    Rtd,
    /// `xlFree`.
    Free,
    /// `xlAsyncReturn`.
    AsyncReturn,
    /// `xlCoerce`.
    Coerce,
}

impl fmt::Display for ExcelApiFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Caller => "xlfCaller",
            Self::SheetName => "xlSheetNm",
            Self::SheetId => "xlSheetId",
            Self::GetName => "xlGetName",
            Self::Register => "xlfRegister",
            Self::Unregister => "xlfUnregister",
            Self::SetName => "xlfSetName",
            Self::Evaluate => "xlfEvaluate",
            Self::EventRegister => "xlEventRegister",
            Self::Rtd => "xlfRtd",
            Self::Free => "xlFree",
            Self::AsyncReturn => "xlAsyncReturn",
            Self::Coerce => "xlCoerce",
        })
    }
}

/// The callback failure or protocol violation associated with an Excel API call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ExcelApiFailure {
    /// Excel returned a failure status.
    Status(ExcelCallbackStatus),
    /// A preceding terminal status prevented this callback.
    Suppressed(ExcelCallbackStatus),
    /// Excel returned a value with an unexpected type or structure.
    UnexpectedResult,
    /// Excel returned an invalid function registration identifier.
    InvalidRegistrationId(i32),
    /// The callback outcome cannot be determined from its return status.
    Indeterminate(ExcelCallbackStatus),
}

impl fmt::Display for ExcelApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(f, "status {status}"),
            Self::Suppressed(status) => write!(f, "suppressed with status {status}"),
            Self::UnexpectedResult => write!(f, "unexpected callback result"),
            Self::InvalidRegistrationId(id) => write!(f, "invalid registration id {id}"),
            Self::Indeterminate(status) => write!(f, "indeterminate callback result {status}"),
        }
    }
}

/// A calculation or framework failure with a safe worksheet representation.
///
/// [`Self::excel_error`] selects the value returned to Excel. The full error is
/// available to diagnostics. Framework-owned variants are non-exhaustive so
/// applications can inspect their context without constructing internal failures.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum XllError {
    /// A named worksheet argument failed validation or conversion.
    #[error("invalid argument {argument}: {reason}")]
    Input {
        /// The argument name declared by the worksheet function.
        argument: &'static str,
        /// The validation or conversion failure.
        reason: InputError,
    },
    /// An array's dimensions differ from the required dimensions.
    #[error(
        "shape mismatch: expected {}x{}, got {}x{}",
        .expected.rows,
        .expected.columns,
        .actual.rows,
        .actual.columns
    )]
    Shape {
        /// The required dimensions.
        expected: Shape,
        /// The supplied dimensions.
        actual: Shape,
    },
    /// The flat storage length differs from the matrix dimensions' product.
    #[error(
        "element count mismatch for {rows}x{columns} matrix: expected {expected}, got {actual}"
    )]
    ElementCountMismatch {
        /// The declared row count.
        rows: usize,
        /// The declared column count.
        columns: usize,
        /// The required number of elements.
        expected: usize,
        /// The supplied number of elements.
        actual: usize,
    },
    /// A calculation failed within its mathematical or numeric domain.
    #[error("domain error: {code:?}")]
    Domain {
        /// A stable category for the calculation failure.
        code: DomainErrorCode,
    },
    /// An application-selected worksheet error with diagnostic-only text.
    ///
    /// Construct this with [`Self::custom`]. The message is never returned as a
    /// worksheet string; it is included in diagnostic Display and Debug output.
    #[error("{message}")]
    Custom {
        /// The worksheet error returned to Excel.
        excel: ExcelError,
        /// Application diagnostic detail.
        message: Cow<'static, str>,
    },
    /// An Excel C API callback failed or violated its result contract.
    #[non_exhaustive]
    #[error("Excel API {function} failed: {failure}")]
    ExcelApi {
        /// The callback that failed.
        function: ExcelApiFunction,
        /// The returned status or detected protocol violation.
        failure: ExcelApiFailure,
    },
    /// A framework Windows API operation failed.
    #[non_exhaustive]
    #[error("Windows API {function} failed with {code}")]
    WindowsApi {
        /// The Windows API operation.
        function: &'static str,
        /// Its error code or HRESULT.
        code: i32,
    },
    /// A registration would overwrite an existing Excel name.
    #[non_exhaustive]
    #[error("Excel name {name} is already registered")]
    RegistrationConflict {
        /// The conflicting name.
        name: &'static str,
    },
    /// Registration cleanup found that an Excel name's binding changed.
    #[non_exhaustive]
    #[error("Excel name {name} no longer refers to the expected registration")]
    MetadataDebtBindingChanged {
        /// The name whose binding no longer belongs to this registration.
        name: &'static str,
    },
    /// The framework could not load a required native library.
    #[non_exhaustive]
    #[error("failed to load {} (OS error {os_error})", path.display())]
    LibraryLoad {
        /// The requested library path.
        path: PathBuf,
        /// The operating system's loader error code.
        os_error: u32,
    },
    /// A required export was absent from a native library.
    #[non_exhaustive]
    #[error("missing symbol {symbol}")]
    MissingSymbol {
        /// The missing export name.
        symbol: &'static str,
    },
    /// A native component's ABI version differs from the required version.
    #[non_exhaustive]
    #[error("ABI mismatch: expected {expected}, got {actual}")]
    AbiMismatch {
        /// The required ABI version.
        expected: u32,
        /// The reported ABI version.
        actual: u32,
    },
    /// An application or operating-system operation failed.
    ///
    /// `std::io::Error` converts to this variant, retaining its raw OS code or
    /// zero when the error has no raw code, and its formatted diagnostic text.
    #[error("native error {code}: {message}")]
    Native {
        /// An application or raw operating-system error code.
        code: i32,
        /// Diagnostic text describing the failure.
        message: String,
    },
    /// Framework cleanup could not quiesce an RTD subscription.
    #[non_exhaustive]
    #[error(
        "RTD subscription shutdown failed for server generation {server_generation}, topic {topic_id}, key {key}: {source}"
    )]
    RtdSubscriptionShutdown {
        /// The server generation owning the subscription.
        server_generation: u64,
        /// The Excel topic identifier.
        topic_id: i32,
        /// The source-specific subscription key.
        key: String,
        /// The subscription's cleanup failure.
        #[source]
        source: Box<XllError>,
    },
    /// An RTD producer stopped with a failure.
    #[non_exhaustive]
    #[error("RTD producer failed for topic {topic}: {source}")]
    RtdProducerFailure {
        /// The failing producer's topic.
        topic: String,
        /// The original producer failure.
        #[source]
        source: Box<XllError>,
    },
    /// An intentional worksheet error or preserved input Excel error.
    #[error("Excel error value {0}")]
    ExcelValue(ExcelError),
    /// An incoming handle does not have a valid identity or type.
    #[error("invalid handle")]
    InvalidHandle,
    /// A handle's object or generation is no longer live.
    #[error("stale handle")]
    StaleHandle,
    /// The add-in or requested service is closing or closed.
    #[error("add-in is closing")]
    Closing,
    /// A bounded runtime service cannot admit more work.
    #[error("runtime capacity is exhausted")]
    Overloaded,
    /// A reentrant operation would block waiting for itself.
    #[error("reentrant call would wait for itself")]
    ReentrantCall,
    /// The framework caught a panic before it could cross the XLL boundary.
    #[error("panic was caught at the XLL boundary")]
    Panic,
    /// An internal invariant or environmental failure has a correlation code.
    #[non_exhaustive]
    #[error("internal error (diagnostic {diagnostic_id:016x})")]
    Internal {
        /// The framework diagnostic identifier for the failure.
        diagnostic_id: DiagnosticId,
    },
}

impl XllError {
    /// Creates a validation error associated with a worksheet argument.
    #[must_use]
    pub const fn input(argument: &'static str, reason: InputError) -> Self {
        Self::Input { argument, reason }
    }

    /// Creates an application error with a chosen worksheet value and diagnostic text.
    ///
    /// Borrowed static text is retained without allocation. Owned messages are
    /// subject to the same bounded diagnostic queue and tracing limits as other
    /// errors. The worksheet receives only `excel`.
    #[must_use]
    pub fn custom(excel: ExcelError, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Custom {
            excel,
            message: message.into(),
        }
    }

    /// Returns the safe worksheet error for this failure.
    #[must_use]
    pub const fn excel_error(&self) -> ExcelError {
        match self {
            Self::RtdProducerFailure { source, .. } => source.excel_error(),
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
            Self::ExcelValue(error) | Self::Custom { excel: error, .. } => *error,
            Self::Input { .. }
            | Self::Shape { .. }
            | Self::ElementCountMismatch { .. }
            | Self::ExcelApi { .. }
            | Self::WindowsApi { .. }
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
    fn excel_error_conversion_and_display_preserve_every_supported_code() {
        for (error, text) in [
            (ExcelError::Null, "#NULL!"),
            (ExcelError::DivisionByZero, "#DIV/0!"),
            (ExcelError::Value, "#VALUE!"),
            (ExcelError::Reference, "#REF!"),
            (ExcelError::Name, "#NAME?"),
            (ExcelError::Number, "#NUM!"),
            (ExcelError::NotAvailable, "#N/A"),
            (ExcelError::GettingData, "#GETTING_DATA"),
        ] {
            assert_eq!(ExcelError::from_code(error.code()), Some(error));
            assert_eq!(XllError::from(error).excel_error(), error);
            assert_eq!(error.into_xll_error().excel_error(), error);
            assert_eq!(error.to_string(), text);
        }
        assert_eq!(ExcelError::from_code(i32::MAX), None);
    }

    #[test]
    fn io_conversion_preserves_os_code_and_message_without_a_synthetic_code() {
        let original = io::Error::from_raw_os_error(42);
        let text = original.to_string();
        let XllError::Native { code, message } = XllError::from(original) else {
            panic!("I/O errors must preserve their native context");
        };
        assert_eq!(code, 42);
        assert_eq!(message, text);

        let error = io::Error::other("application I/O failure").into_xll_error();
        assert!(matches!(
            error,
            XllError::Native { code: 0, ref message } if message == "application I/O failure"
        ));
        assert_eq!(error.excel_error(), ExcelError::Value);
    }

    #[test]
    fn diagnostic_initialization_conversion_retains_lifecycle_and_worker_context() {
        use crate::diagnostics::DiagnosticInitError;
        assert!(matches!(
            DiagnosticInitError::ReentrantMutation.into_xll_error(),
            XllError::ReentrantCall
        ));
        assert!(matches!(
            XllError::from(DiagnosticInitError::RouterClosed),
            XllError::Closing
        ));
        let error = XllError::from(DiagnosticInitError::WorkerSpawn(io::Error::other(
            "worker resource limit",
        )));
        assert_eq!(error.excel_error(), ExcelError::Value);
        assert_eq!(
            error.to_string(),
            "failed to start diagnostic logger worker: worker resource limit"
        );
    }

    #[test]
    fn application_error_selects_the_cell_error_without_losing_diagnostic_text() {
        let error = XllError::custom(ExcelError::NotAvailable, "market data unavailable");
        assert_eq!(error.excel_error(), ExcelError::NotAvailable);
        assert_eq!(error.to_string(), "market data unavailable");
        assert!(matches!(
            error,
            XllError::Custom {
                message: Cow::Borrowed(_),
                ..
            }
        ));
        let error = XllError::RtdProducerFailure {
            topic: "last:EXAMPLE".to_owned(),
            source: Box::new(XllError::custom(
                ExcelError::Reference,
                "upstream reference",
            )),
        };
        assert_eq!(error.excel_error(), ExcelError::Reference);
    }

    #[test]
    fn input_error_display_is_readable_at_the_boundary() {
        assert_eq!(
            XllError::input("value", InputError::NonFinite).to_string(),
            "invalid argument value: number is not finite"
        );
        assert_eq!(
            InputError::TooLarge {
                limit: 10,
                actual: 11
            }
            .to_string(),
            "input exceeds limit 10: got 11"
        );
        fn accepts_standard_error(_: &dyn std::error::Error) {}
        accepts_standard_error(&InputError::InvalidUtf16);
        accepts_standard_error(&ExcelError::NotAvailable);
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
}
