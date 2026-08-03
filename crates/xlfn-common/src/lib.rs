//! Small validation rules shared by the runtime and packaging layers.

#![deny(unsafe_code)]

use std::fmt;

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
}
