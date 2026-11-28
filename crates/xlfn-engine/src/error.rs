//! Worksheet error values, boundary failures, and application error conversion.
//!
//! [`XllError`] keeps diagnostic detail separate from the [`ExcelError`] returned
//! to a worksheet. Applications can return `Result<T, ExcelError>` directly, use
//! [`UdfError`] for application messages and original causes, or implement
//! [`IntoXllError`] for an application-specific error type.

use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::diagnostic_id::DiagnosticId;
use crate::value_types::{Shape, XlValueType};

/// A calculation or lifecycle result with framework diagnostic detail.
pub type XllResult<T> = Result<T, XllError>;

/// A failure to create, install, or replace a diagnostic sink.
///
/// Converts into [`XllError`] with `From` and [`crate::error::IntoXllError`].
/// I/O failures retain their operating-system context; router lifecycle failures
/// become closing or reentrancy errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DiagnosticInitError {
    /// A process-wide sink is already installed; replacement must be explicit.
    #[error("a process-wide diagnostic sink is already installed")]
    AlreadyInstalled,
    /// The diagnostic file or another sink resource could not be initialized.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The bounded diagnostic delivery worker could not be started.
    #[error("failed to start diagnostic logger worker: {0}")]
    WorkerSpawn(#[source] io::Error),
    /// The diagnostic worker attempted to replace its own sink.
    #[error("diagnostic sink mutation was requested from its own worker")]
    ReentrantMutation,
    /// The process diagnostic router is closing or closed.
    #[error("the diagnostic router is closing or closed")]
    RouterClosed,
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
#[repr(i32)]
pub enum ExcelError {
    /// `#NULL!`: an invalid intersection of references.
    Null = 0,
    /// `#DIV/0!`: division by zero.
    DivisionByZero = 7,
    /// `#VALUE!`: an invalid value, type, or structure.
    Value = 15,
    /// `#REF!`: an invalid cell reference.
    Reference = 23,
    /// `#NAME?`: an unrecognized name.
    Name = 29,
    /// `#NUM!`: a numeric or domain failure.
    Number = 36,
    /// `#N/A`: a result is unavailable.
    NotAvailable = 42,
    /// `#GETTING_DATA`: Excel is waiting for external data.
    GettingData = 43,
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
            0 => Some(Self::Null),
            7 => Some(Self::DivisionByZero),
            15 => Some(Self::Value),
            23 => Some(Self::Reference),
            29 => Some(Self::Name),
            36 => Some(Self::Number),
            42 => Some(Self::NotAvailable),
            43 => Some(Self::GettingData),
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
        Self::excel_value(error)
    }
}

impl IntoXllError for ExcelError {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

impl From<io::Error> for XllError {
    fn from(error: io::Error) -> Self {
        Self::native(error.raw_os_error().unwrap_or(0), error.to_string())
    }
}

impl IntoXllError for io::Error {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

impl From<crate::error::DiagnosticInitError> for XllError {
    fn from(error: crate::error::DiagnosticInitError) -> Self {
        match error {
            DiagnosticInitError::Io(error) => error.into(),
            error @ (DiagnosticInitError::WorkerSpawn(_)
            | DiagnosticInitError::AlreadyInstalled) => {
                Self::custom(ExcelError::Value, error.to_string())
            }
            DiagnosticInitError::ReentrantMutation => Self::REENTRANT_CALL,
            DiagnosticInitError::RouterClosed => Self::CLOSING,
        }
    }
}

impl IntoXllError for crate::error::DiagnosticInitError {
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
    /// A string does not match any declared Excel enum value.
    #[error("expected one of {expected}, received value redacted")]
    UnknownEnum {
        /// The received value, possibly truncated by the generated decoder.
        actual: String,
        /// The declared candidates, possibly truncated for large enums.
        expected: &'static str,
    },
    /// An input exceeds a declared element or byte limit.
    #[error("input exceeds limit {limit}: got {actual}")]
    TooLarge {
        /// The maximum accepted size.
        limit: usize,
        /// The incoming size.
        actual: usize,
    },
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
    /// `xlfGetDocument`.
    GetDocument,
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
            Self::GetDocument => "xlfGetDocument",
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

/// Application-facing error with a selected worksheet value and original cause.
///
/// UDFs may return [`UdfResult`]. The original source remains inspectable until
/// conversion at the Excel boundary. Conversion snapshots at most eight causes
/// and 1024 UTF-8 bytes per cause; diagnostics never retain arbitrary application
/// objects. The main message is limited to 4096 bytes at that boundary.
#[derive(Debug)]
pub struct UdfError {
    excel: ExcelError,
    message: Cow<'static, str>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

/// A calculation result with an application-facing error.
pub type UdfResult<T> = Result<T, UdfError>;

impl UdfError {
    /// Creates an application failure; diagnostic text is never a worksheet string.
    pub fn new(excel: ExcelError, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            excel,
            message: message.into(),
            source: None,
        }
    }

    /// Preserves an original error for inspection before the Excel boundary.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Returns the selected worksheet error.
    pub const fn to_excel_error(&self) -> ExcelError {
        self.excel
    }

    /// Returns application diagnostic text.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for UdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &dyn std::error::Error)
    }
}

impl From<io::Error> for UdfError {
    fn from(source: io::Error) -> Self {
        Self::new(ExcelError::Value, "I/O operation failed").with_source(source)
    }
}

impl From<ExcelError> for UdfError {
    fn from(error: ExcelError) -> Self {
        Self::new(error, "worksheet error")
    }
}

impl From<XllError> for UdfError {
    fn from(error: XllError) -> Self {
        Self::new(error.to_excel_error(), "framework operation failed").with_source(error)
    }
}

/// Bounded diagnostic copy of one application cause.
///
/// This preserves cause text and order, not the original concrete error type.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct ErrorCause {
    message: String,
    #[source]
    source: Option<Box<ErrorCause>>,
}

impl ErrorCause {
    fn snapshot(source: &(dyn std::error::Error + 'static), remaining: usize) -> Self {
        Self {
            message: bounded_error_text(format_args!("{source}"), 1024),
            source: if remaining > 1 {
                source
                    .source()
                    .map(|next| Box::new(Self::snapshot(next, remaining - 1)))
            } else {
                None
            },
        }
    }

    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.message.len()
            + self
                .source
                .as_ref()
                .map_or(0, |source| source.retained_bytes())
    }
}

fn bounded_error_text(arguments: std::fmt::Arguments<'_>, limit: usize) -> String {
    struct Text {
        text: String,
        limit: usize,
    }
    impl std::fmt::Write for Text {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let end = text.floor_char_boundary(text.len().min(self.limit - self.text.len()));
            self.text.push_str(&text[..end]);
            if end == text.len() {
                Ok(())
            } else {
                Err(std::fmt::Error)
            }
        }
    }
    let mut text = Text {
        text: String::new(),
        limit,
    };
    let _ = std::fmt::write(&mut text, arguments);
    text.text
}

impl From<UdfError> for XllError {
    fn from(error: UdfError) -> Self {
        internal::application(
            error.excel,
            bounded_error_text(format_args!("{}", error.message), 4096),
            error
                .source
                .as_deref()
                .map(|source| Box::new(ErrorCause::snapshot(source, 8))),
        )
    }
}

impl IntoXllError for UdfError {
    fn into_xll_error(self) -> XllError {
        self.into()
    }
}

/// A rejected configuration limit, independent of Excel worksheet conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("configuration limit is outside its supported range")]
pub struct InvalidLimit;

impl From<InvalidLimit> for XllError {
    fn from(_: InvalidLimit) -> Self {
        Self::domain(DomainErrorCode::InvalidInput)
    }
}

impl From<InvalidLimit> for UdfError {
    fn from(error: InvalidLimit) -> Self {
        Self::new(ExcelError::Value, "invalid configuration limit").with_source(error)
    }
}

/// Coarse category of an [`XllError`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum XllErrorKind {
    /// Bounded application failure copied at the worksheet boundary.
    Application,
    /// A named worksheet argument failed validation or conversion.
    Input,
    /// An array's dimensions differ from the required dimensions.
    Shape,
    /// The flat storage length differs from the matrix dimensions' product.
    ElementCountMismatch,
    /// A calculation failed within its mathematical or numeric domain.
    Domain,
    /// An application-selected worksheet error with diagnostic-only text.
    Custom,
    /// An Excel C API callback failed or violated its result contract.
    ExcelApi,
    /// A framework Windows API operation failed.
    WindowsApi,
    /// A registration would overwrite an existing Excel name.
    RegistrationConflict,
    /// Registration cleanup found that an Excel name's binding changed.
    MetadataDebtBindingChanged,
    /// The framework could not load a required native library.
    LibraryLoad,
    /// A required export was absent from a native library.
    MissingSymbol,
    /// A native component's ABI version differs from the required version.
    AbiMismatch,
    /// An application or operating-system operation failed.
    Native,
    /// Framework cleanup could not quiesce an RTD subscription.
    RtdSubscriptionShutdown,
    /// An RTD producer stopped with a failure.
    RtdProducerFailure,
    /// An intentional worksheet error or preserved input Excel error.
    ExcelValue,
    /// An incoming handle does not have a valid identity or type.
    InvalidHandle,
    /// A handle's object or generation is no longer live.
    StaleHandle,
    /// The add-in or requested service is closing or closed.
    Closing,
    /// A bounded runtime service cannot admit more work.
    Overloaded,
    /// A reentrant operation would block waiting for itself.
    ReentrantCall,
    /// The framework caught a panic before it could cross the XLL boundary.
    Panic,
    /// An internal invariant or environmental failure has a correlation code.
    Internal,
}

#[derive(Clone)]
enum XllErrorRepr {
    // Inline variants: zero allocation, OOM-safe, const-constructible.
    Domain(DomainErrorCode),
    ExcelApi {
        function: ExcelApiFunction,
        failure: ExcelApiFailure,
    },
    AbiMismatch {
        expected: u32,
        actual: u32,
    },
    ExcelValue(ExcelError),
    InvalidHandle,
    StaleHandle,
    Closing,
    Overloaded,
    ReentrantCall,
    Panic,
    Internal(DiagnosticId),
    // Boxed variants: keep XllError at 16 bytes so XllResult<T> stays compact.
    Boxed(Box<BoxedXllError>),
}

#[derive(Clone, Debug)]
enum BoxedXllError {
    Application {
        excel: ExcelError,
        message: String,
        source: Option<Box<ErrorCause>>,
    },
    Input {
        argument: &'static str,
        reason: InputError,
    },
    Shape {
        expected: Shape,
        actual: Shape,
    },
    ElementCountMismatch {
        rows: usize,
        columns: usize,
        expected: usize,
        actual: usize,
    },
    Custom {
        excel: ExcelError,
        message: Cow<'static, str>,
    },
    WindowsApi {
        function: &'static str,
        code: i32,
    },
    RegistrationConflict {
        name: &'static str,
    },
    MetadataDebtBindingChanged {
        name: &'static str,
    },
    LibraryLoad {
        path: PathBuf,
        os_error: u32,
    },
    MissingSymbol {
        symbol: &'static str,
    },
    Native {
        code: i32,
        message: String,
    },
    RtdSubscriptionShutdown {
        server_generation: u64,
        topic_id: i32,
        key: String,
        source: XllError,
    },
    RtdProducerFailure {
        topic: String,
        source: XllError,
    },
}

/// A calculation or framework failure with a safe worksheet representation.
///
/// [`Self::to_excel_error`] selects the value returned to Excel. The full error is
/// available to diagnostics. Lightweight framework errors (such as closing,
/// overload, panic, and Excel error values) are stored inline without heap
/// allocation, while detailed diagnostics are boxed so `XllResult<T>` stays compact.
#[derive(Clone)]
pub struct XllError(XllErrorRepr);

const _: () = assert!(size_of::<XllError>() == 16);

impl fmt::Debug for XllError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            XllErrorRepr::Domain(code) => f.debug_struct("Domain").field("code", code).finish(),
            XllErrorRepr::ExcelApi { function, failure } => f
                .debug_struct("ExcelApi")
                .field("function", function)
                .field("failure", failure)
                .finish(),
            XllErrorRepr::AbiMismatch { expected, actual } => f
                .debug_struct("AbiMismatch")
                .field("expected", expected)
                .field("actual", actual)
                .finish(),
            XllErrorRepr::ExcelValue(error) => f.debug_tuple("ExcelValue").field(error).finish(),
            XllErrorRepr::InvalidHandle => f.write_str("InvalidHandle"),
            XllErrorRepr::StaleHandle => f.write_str("StaleHandle"),
            XllErrorRepr::Closing => f.write_str("Closing"),
            XllErrorRepr::Overloaded => f.write_str("Overloaded"),
            XllErrorRepr::ReentrantCall => f.write_str("ReentrantCall"),
            XllErrorRepr::Panic => f.write_str("Panic"),
            XllErrorRepr::Internal(diagnostic_id) => f
                .debug_struct("Internal")
                .field("diagnostic_id", diagnostic_id)
                .finish(),
            XllErrorRepr::Boxed(boxed) => fmt::Debug::fmt(boxed.as_ref(), f),
        }
    }
}

impl fmt::Display for XllError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            XllErrorRepr::Domain(code) => write!(f, "domain error: {code:?}"),
            XllErrorRepr::ExcelApi { function, failure } => {
                write!(f, "Excel API {function} failed: {failure}")
            }
            XllErrorRepr::AbiMismatch { expected, actual } => {
                write!(f, "ABI mismatch: expected {expected}, got {actual}")
            }
            XllErrorRepr::ExcelValue(error) => write!(f, "Excel error value {error}"),
            XllErrorRepr::InvalidHandle => f.write_str("invalid handle"),
            XllErrorRepr::StaleHandle => f.write_str("stale handle"),
            XllErrorRepr::Closing => f.write_str("add-in is closing"),
            XllErrorRepr::Overloaded => f.write_str("runtime capacity is exhausted"),
            XllErrorRepr::ReentrantCall => f.write_str("reentrant call would wait for itself"),
            XllErrorRepr::Panic => f.write_str("panic was caught at the XLL boundary"),
            XllErrorRepr::Internal(diagnostic_id) => {
                write!(f, "internal error (diagnostic {diagnostic_id:016x})")
            }
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Application { message, .. } => f.write_str(message),
                BoxedXllError::Input { argument, reason } => {
                    write!(f, "invalid argument {argument}: {reason}")
                }
                BoxedXllError::Shape { expected, actual } => write!(
                    f,
                    "shape mismatch: expected {}x{}, got {}x{}",
                    expected.rows, expected.columns, actual.rows, actual.columns
                ),
                BoxedXllError::ElementCountMismatch {
                    rows,
                    columns,
                    expected,
                    actual,
                } => write!(
                    f,
                    "element count mismatch for {rows}x{columns} matrix: expected {expected}, got {actual}"
                ),
                BoxedXllError::Custom { message, .. } => f.write_str(message),
                BoxedXllError::WindowsApi { function, code } => {
                    write!(f, "Windows API {function} failed with {code}")
                }
                BoxedXllError::RegistrationConflict { name } => {
                    write!(f, "Excel name {name} is already registered")
                }
                BoxedXllError::MetadataDebtBindingChanged { name } => {
                    write!(
                        f,
                        "Excel name {name} no longer refers to the expected registration"
                    )
                }
                BoxedXllError::LibraryLoad { path, os_error } => {
                    write!(f, "failed to load {} (OS error {os_error})", path.display())
                }
                BoxedXllError::MissingSymbol { symbol } => write!(f, "missing symbol {symbol}"),
                BoxedXllError::Native { code, message } => {
                    write!(f, "native error {code}: {message}")
                }
                BoxedXllError::RtdSubscriptionShutdown {
                    server_generation,
                    topic_id,
                    key,
                    source,
                } => write!(
                    f,
                    "RTD subscription shutdown failed for server generation {server_generation}, topic {topic_id}, key {key}: {source}"
                ),
                BoxedXllError::RtdProducerFailure { topic, source } => {
                    write!(f, "RTD producer failed for topic {topic}: {source}")
                }
            },
        }
    }
}

impl std::error::Error for XllError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Application { source, .. } => source
                    .as_deref()
                    .map(|source| source as &dyn std::error::Error),
                BoxedXllError::RtdSubscriptionShutdown { source, .. }
                | BoxedXllError::RtdProducerFailure { source, .. } => Some(source),
                _ => None,
            },
            _ => None,
        }
    }
}

impl XllError {
    /// An incoming handle does not have a valid identity or type.
    pub const INVALID_HANDLE: Self = Self(XllErrorRepr::InvalidHandle);
    /// A handle's object or generation is no longer live.
    pub const STALE_HANDLE: Self = Self(XllErrorRepr::StaleHandle);
    /// The add-in or requested service is closing or closed.
    pub const CLOSING: Self = Self(XllErrorRepr::Closing);
    /// A bounded runtime service cannot admit more work.
    pub const OVERLOADED: Self = Self(XllErrorRepr::Overloaded);
    /// A reentrant operation would block waiting for itself.
    pub const REENTRANT_CALL: Self = Self(XllErrorRepr::ReentrantCall);
    /// The framework caught a panic before it could cross the XLL boundary.
    pub const PANIC: Self = Self(XllErrorRepr::Panic);

    /// Creates an intentional worksheet error or preserved input Excel error.
    #[must_use]
    pub const fn excel_value(error: ExcelError) -> Self {
        Self(XllErrorRepr::ExcelValue(error))
    }

    /// Creates a domain error with the corresponding worksheet `#NUM!` value.
    #[must_use]
    pub const fn domain(code: DomainErrorCode) -> Self {
        Self(XllErrorRepr::Domain(code))
    }

    /// Creates a validation error associated with a worksheet argument.
    #[must_use]
    pub fn input(argument: &'static str, reason: InputError) -> Self {
        Self(XllErrorRepr::Boxed(Box::new(BoxedXllError::Input {
            argument,
            reason,
        })))
    }

    /// Creates an array dimension mismatch error.
    #[must_use]
    pub fn shape(expected: Shape, actual: Shape) -> Self {
        Self(XllErrorRepr::Boxed(Box::new(BoxedXllError::Shape {
            expected,
            actual,
        })))
    }

    /// Creates a matrix element count mismatch error.
    #[must_use]
    pub fn element_count_mismatch(
        rows: usize,
        columns: usize,
        expected: usize,
        actual: usize,
    ) -> Self {
        Self(XllErrorRepr::Boxed(Box::new(
            BoxedXllError::ElementCountMismatch {
                rows,
                columns,
                expected,
                actual,
            },
        )))
    }

    /// Creates an application or operating-system native error.
    #[must_use]
    pub fn native(code: i32, message: impl Into<String>) -> Self {
        Self(XllErrorRepr::Boxed(Box::new(BoxedXllError::Native {
            code,
            message: message.into(),
        })))
    }

    /// Reports validation of a named resource rather than a worksheet parameter.
    /// The reason is retained as a bounded diagnostic cause.
    pub fn validation(context: &'static str, reason: InputError) -> Self {
        let excel = if matches!(reason, InputError::NumericOverflow) {
            ExcelError::Number
        } else {
            ExcelError::Value
        };
        UdfError::new(excel, format!("invalid {context}"))
            .with_source(reason)
            .into()
    }

    /// Attaches the caller's parameter name to an input validation failure.
    /// Other error categories retain their existing context.
    pub fn with_argument(mut self, argument: &'static str) -> Self {
        if let XllErrorRepr::Boxed(boxed) = &mut self.0
            && let BoxedXllError::Input { argument: slot, .. } = boxed.as_mut()
        {
            *slot = argument;
        }
        self
    }

    /// Creates an application error with a chosen worksheet value and diagnostic text.
    ///
    /// Borrowed static text is retained without an extra string copy. Owned messages
    /// are subject to the same bounded diagnostic queue and tracing limits as other
    /// errors. The worksheet receives only `excel`.
    #[must_use]
    pub fn custom(excel: ExcelError, message: impl Into<Cow<'static, str>>) -> Self {
        Self(XllErrorRepr::Boxed(Box::new(BoxedXllError::Custom {
            excel,
            message: message.into(),
        })))
    }

    /// Returns the coarse category of this failure.
    #[must_use]
    pub fn kind(&self) -> XllErrorKind {
        match &self.0 {
            XllErrorRepr::Domain(_) => XllErrorKind::Domain,
            XllErrorRepr::ExcelApi { .. } => XllErrorKind::ExcelApi,
            XllErrorRepr::AbiMismatch { .. } => XllErrorKind::AbiMismatch,
            XllErrorRepr::ExcelValue(_) => XllErrorKind::ExcelValue,
            XllErrorRepr::InvalidHandle => XllErrorKind::InvalidHandle,
            XllErrorRepr::StaleHandle => XllErrorKind::StaleHandle,
            XllErrorRepr::Closing => XllErrorKind::Closing,
            XllErrorRepr::Overloaded => XllErrorKind::Overloaded,
            XllErrorRepr::ReentrantCall => XllErrorKind::ReentrantCall,
            XllErrorRepr::Panic => XllErrorKind::Panic,
            XllErrorRepr::Internal(_) => XllErrorKind::Internal,
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Application { .. } => XllErrorKind::Application,
                BoxedXllError::Input { .. } => XllErrorKind::Input,
                BoxedXllError::Shape { .. } => XllErrorKind::Shape,
                BoxedXllError::ElementCountMismatch { .. } => XllErrorKind::ElementCountMismatch,
                BoxedXllError::Custom { .. } => XllErrorKind::Custom,
                BoxedXllError::WindowsApi { .. } => XllErrorKind::WindowsApi,
                BoxedXllError::RegistrationConflict { .. } => XllErrorKind::RegistrationConflict,
                BoxedXllError::MetadataDebtBindingChanged { .. } => {
                    XllErrorKind::MetadataDebtBindingChanged
                }
                BoxedXllError::LibraryLoad { .. } => XllErrorKind::LibraryLoad,
                BoxedXllError::MissingSymbol { .. } => XllErrorKind::MissingSymbol,
                BoxedXllError::Native { .. } => XllErrorKind::Native,
                BoxedXllError::RtdSubscriptionShutdown { .. } => {
                    XllErrorKind::RtdSubscriptionShutdown
                }
                BoxedXllError::RtdProducerFailure { .. } => XllErrorKind::RtdProducerFailure,
            },
        }
    }

    /// Returns whether this error indicates an invalid handle.
    #[must_use]
    pub const fn is_invalid_handle(&self) -> bool {
        matches!(self.0, XllErrorRepr::InvalidHandle)
    }

    /// Returns whether this error indicates a stale handle.
    #[must_use]
    pub const fn is_stale_handle(&self) -> bool {
        matches!(self.0, XllErrorRepr::StaleHandle)
    }

    /// Returns whether this error indicates that the add-in or service is closing.
    #[must_use]
    pub const fn is_closing(&self) -> bool {
        matches!(self.0, XllErrorRepr::Closing)
    }

    /// Returns whether this error indicates runtime capacity exhaustion.
    #[must_use]
    pub const fn is_overloaded(&self) -> bool {
        matches!(self.0, XllErrorRepr::Overloaded)
    }

    /// Returns whether this error indicates a reentrant call conflict.
    #[must_use]
    pub const fn is_reentrant_call(&self) -> bool {
        matches!(self.0, XllErrorRepr::ReentrantCall)
    }

    /// Returns whether this error indicates a caught panic.
    #[must_use]
    pub const fn is_panic(&self) -> bool {
        matches!(self.0, XllErrorRepr::Panic)
    }

    /// Returns the domain error code if this is a domain error.
    #[must_use]
    pub const fn domain_code(&self) -> Option<DomainErrorCode> {
        match self.0 {
            XllErrorRepr::Domain(code) => Some(code),
            _ => None,
        }
    }

    /// Returns the worksheet error value if this is an `ExcelValue` error.
    #[must_use]
    pub const fn excel_value_error(&self) -> Option<ExcelError> {
        match self.0 {
            XllErrorRepr::ExcelValue(error) => Some(error),
            _ => None,
        }
    }

    /// Returns the argument name and reason if this is an input validation error.
    #[must_use]
    pub fn input_details(&self) -> Option<(&'static str, &InputError)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Input { argument, reason } => Some((*argument, reason)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns `(expected, actual)` if this is a shape mismatch error.
    #[must_use]
    pub fn shape_details(&self) -> Option<(Shape, Shape)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Shape { expected, actual } => Some((*expected, *actual)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns `(code, message)` if this is a native error.
    #[must_use]
    pub fn native_details(&self) -> Option<(i32, &str)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Native { code, message } => Some((*code, message.as_str())),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns `(function, code)` if this is a Windows API error.
    #[must_use]
    pub fn windows_api_details(&self) -> Option<(&'static str, i32)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::WindowsApi { function, code } => Some((*function, *code)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns `(function, failure)` if this is an Excel API error.
    #[must_use]
    pub const fn excel_api_details(&self) -> Option<(ExcelApiFunction, ExcelApiFailure)> {
        match self.0 {
            XllErrorRepr::ExcelApi { function, failure } => Some((function, failure)),
            _ => None,
        }
    }

    /// Returns `(excel, message)` if this is a custom application error.
    #[must_use]
    pub fn custom_details(&self) -> Option<(ExcelError, &Cow<'static, str>)> {
        match &self.0 {
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Custom { excel, message } => Some((*excel, message)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Returns the diagnostic ID if this is an internal framework error.
    #[must_use]
    pub const fn diagnostic_id(&self) -> Option<DiagnosticId> {
        match self.0 {
            XllErrorRepr::Internal(id) => Some(id),
            _ => None,
        }
    }

    /// Returns the safe worksheet error for this failure.
    #[must_use]
    pub fn to_excel_error(&self) -> ExcelError {
        match &self.0 {
            XllErrorRepr::Domain(_) => ExcelError::Number,
            XllErrorRepr::InvalidHandle
            | XllErrorRepr::StaleHandle
            | XllErrorRepr::Closing
            | XllErrorRepr::Overloaded
            | XllErrorRepr::ReentrantCall => ExcelError::NotAvailable,
            XllErrorRepr::ExcelValue(error) => *error,
            XllErrorRepr::ExcelApi { .. }
            | XllErrorRepr::AbiMismatch { .. }
            | XllErrorRepr::Panic
            | XllErrorRepr::Internal(_) => ExcelError::Value,
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::RtdProducerFailure { source, .. } => source.to_excel_error(),
                BoxedXllError::Input {
                    reason: InputError::NumericOverflow,
                    ..
                } => ExcelError::Number,
                BoxedXllError::Custom { excel: error, .. }
                | BoxedXllError::Application { excel: error, .. } => *error,
                BoxedXllError::Input { .. }
                | BoxedXllError::Shape { .. }
                | BoxedXllError::ElementCountMismatch { .. }
                | BoxedXllError::WindowsApi { .. }
                | BoxedXllError::RegistrationConflict { .. }
                | BoxedXllError::MetadataDebtBindingChanged { .. }
                | BoxedXllError::LibraryLoad { .. }
                | BoxedXllError::MissingSymbol { .. }
                | BoxedXllError::Native { .. }
                | BoxedXllError::RtdSubscriptionShutdown { .. } => ExcelError::Value,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_the_documented_excel_values() {
        assert_eq!(
            XllError::input("x", InputError::NonFinite).to_excel_error(),
            ExcelError::Value
        );
        assert_eq!(
            XllError::shape(
                Shape {
                    rows: 1,
                    columns: 1,
                },
                Shape {
                    rows: 2,
                    columns: 1,
                },
            )
            .to_excel_error(),
            ExcelError::Value
        );
        assert_eq!(
            XllError::domain(DomainErrorCode::Overflow).to_excel_error(),
            ExcelError::Number
        );
        assert_eq!(
            XllError::input("x", InputError::NumericOverflow).to_excel_error(),
            ExcelError::Number
        );
        assert_eq!(
            XllError::STALE_HANDLE.to_excel_error(),
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
            assert_eq!(XllError::from(error).to_excel_error(), error);
            assert_eq!(error.into_xll_error().to_excel_error(), error);
            assert_eq!(error.to_string(), text);
        }
        assert_eq!(ExcelError::from_code(i32::MAX), None);
    }

    #[test]
    fn io_conversion_preserves_os_code_and_message_without_a_synthetic_code() {
        let original = io::Error::from_raw_os_error(42);
        let text = original.to_string();
        let error = XllError::from(original);
        let Some((code, message)) = error.native_details() else {
            panic!("I/O errors must preserve their native context");
        };
        assert_eq!(code, 42);
        assert_eq!(message, text);

        let error = io::Error::other("application I/O failure").into_xll_error();
        assert_eq!(error.native_details(), Some((0, "application I/O failure")));
        assert_eq!(error.to_excel_error(), ExcelError::Value);
    }

    #[test]
    fn diagnostic_initialization_conversion_retains_lifecycle_and_worker_context() {
        assert_eq!(
            XllError::from(DiagnosticInitError::AlreadyInstalled).to_excel_error(),
            ExcelError::Value
        );
        assert!(DiagnosticInitError::ReentrantMutation.into_xll_error().is_reentrant_call());
        assert!(XllError::from(DiagnosticInitError::RouterClosed).is_closing());
        let error = XllError::from(DiagnosticInitError::WorkerSpawn(io::Error::other(
            "worker resource limit",
        )));
        assert_eq!(error.to_excel_error(), ExcelError::Value);
        assert_eq!(
            error.to_string(),
            "failed to start diagnostic logger worker: worker resource limit"
        );
    }

    #[test]
    fn application_error_selects_the_cell_error_without_losing_diagnostic_text() {
        let error = XllError::custom(ExcelError::NotAvailable, "market data unavailable");
        assert_eq!(error.to_excel_error(), ExcelError::NotAvailable);
        assert_eq!(error.to_string(), "market data unavailable");
        assert!(matches!(
            error.custom_details(),
            Some((_, Cow::Borrowed(_)))
        ));
        let error = internal::rtd_producer_failure(
            "last:EXAMPLE".to_owned(),
            Box::new(XllError::custom(
                ExcelError::Reference,
                "upstream reference",
            )),
        );
        assert_eq!(error.to_excel_error(), ExcelError::Reference);
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
        let error = internal::rtd_subscription_shutdown(
            7,
            42,
            "stream:test".to_owned(),
            Box::new(XllError::PANIC),
        );
        let message = error.to_string();
        assert!(message.contains("server generation 7"));
        assert!(message.contains("topic 42"));
        assert!(message.contains("stream:test"));
        assert!(message.contains("panic was caught"));
    }
}

/// Version-coupled construction and inspection used by the framework crates, not the facade.
#[doc(hidden)]
pub mod internal {
    use super::*;

    /// Borrowed view of all [`XllError`] variants for internal diagnostics and classification.
    #[derive(Clone, Copy, Debug)]
    pub enum ErrorView<'a> {
        Application {
            excel: ExcelError,
            message: &'a str,
            source: Option<&'a ErrorCause>,
        },
        Input {
            argument: &'static str,
            reason: &'a InputError,
        },
        Shape {
            expected: Shape,
            actual: Shape,
        },
        ElementCountMismatch {
            rows: usize,
            columns: usize,
            expected: usize,
            actual: usize,
        },
        Domain {
            code: DomainErrorCode,
        },
        Custom {
            excel: ExcelError,
            message: &'a Cow<'static, str>,
        },
        ExcelApi {
            function: ExcelApiFunction,
            failure: ExcelApiFailure,
        },
        WindowsApi {
            function: &'static str,
            code: i32,
        },
        RegistrationConflict {
            name: &'static str,
        },
        MetadataDebtBindingChanged {
            name: &'static str,
        },
        LibraryLoad {
            path: &'a PathBuf,
            os_error: u32,
        },
        MissingSymbol {
            symbol: &'static str,
        },
        AbiMismatch {
            expected: u32,
            actual: u32,
        },
        Native {
            code: i32,
            message: &'a str,
        },
        RtdSubscriptionShutdown {
            server_generation: u64,
            topic_id: i32,
            key: &'a str,
            source: &'a XllError,
        },
        RtdProducerFailure {
            topic: &'a str,
            source: &'a XllError,
        },
        ExcelValue(ExcelError),
        InvalidHandle,
        StaleHandle,
        Closing,
        Overloaded,
        ReentrantCall,
        Panic,
        Internal {
            diagnostic_id: DiagnosticId,
        },
    }

    pub fn view(error: &XllError) -> ErrorView<'_> {
        match &error.0 {
            XllErrorRepr::Domain(code) => ErrorView::Domain { code: *code },
            XllErrorRepr::ExcelApi { function, failure } => ErrorView::ExcelApi {
                function: *function,
                failure: *failure,
            },
            XllErrorRepr::AbiMismatch { expected, actual } => ErrorView::AbiMismatch {
                expected: *expected,
                actual: *actual,
            },
            XllErrorRepr::ExcelValue(error) => ErrorView::ExcelValue(*error),
            XllErrorRepr::InvalidHandle => ErrorView::InvalidHandle,
            XllErrorRepr::StaleHandle => ErrorView::StaleHandle,
            XllErrorRepr::Closing => ErrorView::Closing,
            XllErrorRepr::Overloaded => ErrorView::Overloaded,
            XllErrorRepr::ReentrantCall => ErrorView::ReentrantCall,
            XllErrorRepr::Panic => ErrorView::Panic,
            XllErrorRepr::Internal(diagnostic_id) => ErrorView::Internal {
                diagnostic_id: *diagnostic_id,
            },
            XllErrorRepr::Boxed(boxed) => match boxed.as_ref() {
                BoxedXllError::Application {
                    excel,
                    message,
                    source,
                } => ErrorView::Application {
                    excel: *excel,
                    message: message.as_str(),
                    source: source.as_deref(),
                },
                BoxedXllError::Input { argument, reason } => ErrorView::Input {
                    argument: *argument,
                    reason,
                },
                BoxedXllError::Shape { expected, actual } => ErrorView::Shape {
                    expected: *expected,
                    actual: *actual,
                },
                BoxedXllError::ElementCountMismatch {
                    rows,
                    columns,
                    expected,
                    actual,
                } => ErrorView::ElementCountMismatch {
                    rows: *rows,
                    columns: *columns,
                    expected: *expected,
                    actual: *actual,
                },
                BoxedXllError::Custom { excel, message } => ErrorView::Custom {
                    excel: *excel,
                    message,
                },
                BoxedXllError::WindowsApi { function, code } => ErrorView::WindowsApi {
                    function: *function,
                    code: *code,
                },
                BoxedXllError::RegistrationConflict { name } => {
                    ErrorView::RegistrationConflict { name: *name }
                }
                BoxedXllError::MetadataDebtBindingChanged { name } => {
                    ErrorView::MetadataDebtBindingChanged { name: *name }
                }
                BoxedXllError::LibraryLoad { path, os_error } => ErrorView::LibraryLoad {
                    path,
                    os_error: *os_error,
                },
                BoxedXllError::MissingSymbol { symbol } => {
                    ErrorView::MissingSymbol { symbol: *symbol }
                }
                BoxedXllError::Native { code, message } => ErrorView::Native {
                    code: *code,
                    message: message.as_str(),
                },
                BoxedXllError::RtdSubscriptionShutdown {
                    server_generation,
                    topic_id,
                    key,
                    source,
                } => ErrorView::RtdSubscriptionShutdown {
                    server_generation: *server_generation,
                    topic_id: *topic_id,
                    key: key.as_str(),
                    source,
                },
                BoxedXllError::RtdProducerFailure { topic, source } => {
                    ErrorView::RtdProducerFailure {
                        topic: topic.as_str(),
                        source,
                    }
                }
            },
        }
    }

    pub const fn boxed_error_size() -> usize {
        size_of::<BoxedXllError>()
    }

    /// Requested dynamic storage of a clone, excluding the fixed channel envelope.
    /// String cloning copies length, not spare capacity. Every nested Box is charged
    /// its full allocation; paths use the platform's lossless encoded byte length.
    pub fn clone_bytes(error: &XllError, max_bytes: usize, max_depth: usize) -> Option<usize> {
        let mut current = error;
        let mut bytes = 0_usize;
        for _ in 0..max_depth {
            let (local, source) = match &current.0 {
                XllErrorRepr::Domain(_)
                | XllErrorRepr::ExcelApi { .. }
                | XllErrorRepr::AbiMismatch { .. }
                | XllErrorRepr::ExcelValue(_)
                | XllErrorRepr::InvalidHandle
                | XllErrorRepr::StaleHandle
                | XllErrorRepr::Closing
                | XllErrorRepr::Overloaded
                | XllErrorRepr::ReentrantCall
                | XllErrorRepr::Panic
                | XllErrorRepr::Internal(_) => (0, None),
                XllErrorRepr::Boxed(boxed) => {
                    let (extra, source) = match boxed.as_ref() {
                        BoxedXllError::Native { message, .. } => (message.len(), None),
                        BoxedXllError::Application {
                            message, source, ..
                        } => (
                            message.len().checked_add(
                                source.as_ref().map_or(0, |source| source.retained_bytes()),
                            )?,
                            None,
                        ),
                        BoxedXllError::Custom { message, .. } => (
                            match message {
                                Cow::Borrowed(_) => 0,
                                Cow::Owned(message) => message.len(),
                            },
                            None,
                        ),
                        BoxedXllError::LibraryLoad { path, .. } => {
                            (path.as_os_str().as_encoded_bytes().len(), None)
                        }
                        BoxedXllError::RtdSubscriptionShutdown { key, source, .. } => {
                            (key.len(), Some(source))
                        }
                        BoxedXllError::RtdProducerFailure { topic, source } => {
                            (topic.len(), Some(source))
                        }
                        BoxedXllError::Input {
                            reason: InputError::UnknownEnum { actual, .. },
                            ..
                        } => (actual.len(), None),
                        BoxedXllError::Input { .. }
                        | BoxedXllError::Shape { .. }
                        | BoxedXllError::ElementCountMismatch { .. }
                        | BoxedXllError::WindowsApi { .. }
                        | BoxedXllError::RegistrationConflict { .. }
                        | BoxedXllError::MetadataDebtBindingChanged { .. }
                        | BoxedXllError::MissingSymbol { .. } => (0, None),
                    };
                    (size_of::<BoxedXllError>().checked_add(extra)?, source)
                }
            };
            bytes = bytes.checked_add(local)?;
            if bytes > max_bytes {
                return None;
            }
            let Some(source) = source else {
                return Some(bytes);
            };
            current = source;
        }
        None
    }

    pub fn application(
        excel: ExcelError,
        message: String,
        source: Option<Box<ErrorCause>>,
    ) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(BoxedXllError::Application {
            excel,
            message,
            source,
        })))
    }

    pub const fn excel_api(function: ExcelApiFunction, failure: ExcelApiFailure) -> XllError {
        XllError(XllErrorRepr::ExcelApi { function, failure })
    }

    pub fn windows_api(function: &'static str, code: i32) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(BoxedXllError::WindowsApi {
            function,
            code,
        })))
    }

    pub fn registration_conflict(name: &'static str) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(
            BoxedXllError::RegistrationConflict { name },
        )))
    }

    pub fn metadata_debt_binding_changed(name: &'static str) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(
            BoxedXllError::MetadataDebtBindingChanged { name },
        )))
    }

    pub fn library_load(path: PathBuf, os_error: u32) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(BoxedXllError::LibraryLoad {
            path,
            os_error,
        })))
    }

    pub fn missing_symbol(symbol: &'static str) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(BoxedXllError::MissingSymbol {
            symbol,
        })))
    }

    pub const fn abi_mismatch(expected: u32, actual: u32) -> XllError {
        XllError(XllErrorRepr::AbiMismatch { expected, actual })
    }

    pub fn rtd_subscription_shutdown(
        server_generation: u64,
        topic_id: i32,
        key: String,
        source: Box<XllError>,
    ) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(
            BoxedXllError::RtdSubscriptionShutdown {
                server_generation,
                topic_id,
                key,
                source: *source,
            },
        )))
    }

    pub fn rtd_producer_failure(topic: String, source: Box<XllError>) -> XllError {
        XllError(XllErrorRepr::Boxed(Box::new(
            BoxedXllError::RtdProducerFailure {
                topic,
                source: *source,
            },
        )))
    }

    pub const fn internal(diagnostic_id: DiagnosticId) -> XllError {
        XllError(XllErrorRepr::Internal(diagnostic_id))
    }
}
