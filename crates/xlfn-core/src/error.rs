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
    Internal { diagnostic_id: u64 },
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
}
