//! Small validation rules shared by the runtime and packaging layers.

#![deny(unsafe_code)]

use std::fmt;

/// The execution capability assigned to a generated Excel function.
///
/// This is the canonical semantic value shared by macro lowering and runtime
/// registration. Keeping the mutually exclusive execution modes in one enum
/// prevents later layers from reconstructing an invalid combination of flags.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ExecutionKind {
    #[default]
    MainThread,
    ThreadSafe,
    MacroSheet,
    Async,
}

impl ExecutionKind {
    #[must_use]
    pub const fn is_async(self) -> bool {
        matches!(self, Self::Async)
    }

    #[must_use]
    pub const fn allows_reference_arguments(self) -> bool {
        matches!(self, Self::MacroSheet)
    }

    #[must_use]
    pub const fn is_thread_safe(self) -> bool {
        matches!(self, Self::ThreadSafe | Self::Async)
    }
}

/// Visibility of a generated Excel registration.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FunctionVisibility {
    #[default]
    Public,
    Hidden,
}

impl FunctionVisibility {
    #[must_use]
    pub const fn macro_type(self) -> f64 {
        match self {
            Self::Public => 1.0,
            Self::Hidden => 0.0,
        }
    }
}

/// Excel's maximum number of visible function arguments.
pub const MAX_EXCEL_FUNCTION_ARGUMENTS: usize = 255;

/// Maximum length of an Excel counted string, measured in UTF-16 code units.
pub const EXCEL_STRING_LIMIT: usize = 32_767;

/// Returns the visible argument limit for one execution mode.
#[must_use]
pub const fn max_excel_function_arguments(execution: ExecutionKind) -> usize {
    if execution.is_async() {
        MAX_EXCEL_FUNCTION_ARGUMENTS - 1
    } else {
        MAX_EXCEL_FUNCTION_ARGUMENTS
    }
}

/// The reason a string cannot be sent as an Excel counted string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExcelStringError {
    TooLong,
    Nul,
}

impl fmt::Display for ExcelStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => formatter.write_str("Excel counted string exceeds the UTF-16 limit"),
            Self::Nul => formatter.write_str("Excel counted string must not contain NUL"),
        }
    }
}

impl std::error::Error for ExcelStringError {}

/// Validates an Excel counted string independently of the host ABI.
pub fn validate_excel_string(value: &str) -> Result<(), ExcelStringError> {
    if value.encode_utf16().count() > EXCEL_STRING_LIMIT {
        return Err(ExcelStringError::TooLong);
    }
    if value.contains('\0') {
        return Err(ExcelStringError::Nul);
    }
    Ok(())
}

/// The reason an Excel argument name is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentNameError {
    Empty,
    ReservedCharacter,
    TooLong,
    CombinedTooLong,
}

impl fmt::Display for ArgumentNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "Excel argument names must not be empty",
            Self::ReservedCharacter => {
                "Excel argument names must not contain comma, NUL, CR, or LF"
            }
            Self::TooLong => "Excel argument name exceeds the UTF-16 limit",
            Self::CombinedTooLong => "combined Excel argument names exceed the UTF-16 limit",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ArgumentNameError {}

/// Validates one Excel argument name.
pub fn validate_argument_name(name: &str) -> Result<(), ArgumentNameError> {
    if name.is_empty() {
        return Err(ArgumentNameError::Empty);
    }
    if name.contains([',', '\0', '\r', '\n']) {
        return Err(ArgumentNameError::ReservedCharacter);
    }
    if name.encode_utf16().count() > EXCEL_STRING_LIMIT {
        return Err(ArgumentNameError::TooLong);
    }
    Ok(())
}

/// Validates argument names individually and as Excel's comma-joined list.
pub fn validate_argument_names(names: &[&str]) -> Result<(), ArgumentNameError> {
    for name in names {
        validate_argument_name(name)?;
    }
    let joined_utf16_len = names
        .iter()
        .map(|name| name.encode_utf16().count())
        .sum::<usize>()
        .saturating_add(names.len().saturating_sub(1));
    if joined_utf16_len > EXCEL_STRING_LIMIT {
        return Err(ArgumentNameError::CombinedTooLong);
    }
    Ok(())
}

/// The reason a string is not a portable Windows basename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsBasenameError {
    Empty,
    DotPath,
    NonAscii,
    TrailingSpace,
    TrailingPeriod,
    ControlCharacter,
    ReservedCharacter,
    ReservedDeviceName,
}

impl fmt::Display for WindowsBasenameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "basename must not be empty",
            Self::DotPath => "basename must not be `.` or `..`",
            Self::NonAscii => "basename must contain only ASCII characters",
            Self::TrailingSpace => "basename must not end with a space",
            Self::TrailingPeriod => "basename must not end with a period",
            Self::ControlCharacter => "basename must not contain control characters",
            Self::ReservedCharacter => "basename contains a reserved Windows filename character",
            Self::ReservedDeviceName => "basename uses a reserved Windows device name",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WindowsBasenameError {}

/// Validates one portable ASCII Windows basename.
///
/// This is the single filename rule shared by runtime add-in identifiers and
/// package entries. Callers may layer a stricter length or naming policy on top
/// of this check, but must not duplicate the Windows device-name rules.
pub fn validate_windows_basename(name: &str) -> Result<(), WindowsBasenameError> {
    if name.is_empty() {
        return Err(WindowsBasenameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(WindowsBasenameError::DotPath);
    }
    if !name.is_ascii() {
        return Err(WindowsBasenameError::NonAscii);
    }
    if name.ends_with(' ') {
        return Err(WindowsBasenameError::TrailingSpace);
    }
    if name.ends_with('.') {
        return Err(WindowsBasenameError::TrailingPeriod);
    }
    if name.chars().any(|character| character <= '\u{1f}') {
        return Err(WindowsBasenameError::ControlCharacter);
    }
    if name
        .chars()
        .any(|character| r#"<>:"/\\|?*"#.contains(character))
    {
        return Err(WindowsBasenameError::ReservedCharacter);
    }

    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem.strip_prefix("COM").is_some_and(|suffix| {
        matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
    }) || stem.strip_prefix("LPT").is_some_and(|suffix| {
        matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
    });
    if reserved {
        return Err(WindowsBasenameError::ReservedDeviceName);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_windows_device_extensions_and_trailing_terminators() {
        for name in [
            "CON.txt",
            "nul.log",
            "LPT1.data",
            "CONIN$",
            "CONOUT$",
            "addin.",
            "addin ",
        ] {
            assert!(validate_windows_basename(name).is_err(), "{name:?} passed");
        }
    }

    #[test]
    fn accepts_a_regular_ascii_basename() {
        assert!(validate_windows_basename("valid-addin_123").is_ok());
    }

    #[test]
    fn execution_kind_owns_the_function_argument_limit() {
        assert_eq!(
            max_excel_function_arguments(ExecutionKind::MainThread),
            MAX_EXCEL_FUNCTION_ARGUMENTS
        );
        assert_eq!(
            max_excel_function_arguments(ExecutionKind::Async),
            MAX_EXCEL_FUNCTION_ARGUMENTS - 1
        );
    }

    #[test]
    fn argument_name_validation_covers_individual_and_joined_limits() {
        assert!(validate_argument_name("value").is_ok());
        assert_eq!(
            validate_argument_name("bad,name"),
            Err(ArgumentNameError::ReservedCharacter)
        );
        assert_eq!(validate_argument_names(&["left", "right"]), Ok(()));
        let long_name = "x".repeat(EXCEL_STRING_LIMIT);
        assert_eq!(
            validate_argument_names(&[long_name.as_str(), "y"]),
            Err(ArgumentNameError::CombinedTooLong)
        );
    }
}
