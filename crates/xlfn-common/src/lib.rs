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

/// Excel's maximum number of visible function arguments.
pub const MAX_EXCEL_FUNCTION_ARGUMENTS: usize = 255;

/// Maximum length of an Excel counted string, measured in UTF-16 code units.
pub const EXCEL_STRING_LIMIT: usize = 32_767;

/// Maximum length of an Excel function name, measured in UTF-16 code units.
pub const EXCEL_FUNCTION_NAME_LIMIT: usize = 255;

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

/// The reason an Excel function name is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionNameError {
    Empty,
    TooLong,
    InvalidStart,
    InvalidCharacter,
    ReservedName,
    CellReference,
}

impl fmt::Display for FunctionNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "Excel function name must not be empty",
            Self::TooLong => "Excel function name exceeds the 255 UTF-16 unit limit",
            Self::InvalidStart => {
                "Excel function name must begin with a letter, `_`, `\\`, or a non-ASCII character"
            }
            Self::InvalidCharacter => "Excel function name contains an invalid character",
            Self::ReservedName => "Excel function name must not be C, R, TRUE, or FALSE",
            Self::CellReference => "Excel function name must not be an A1 or R1C1 reference",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for FunctionNameError {}

/// Validates the length and formula syntax of an Excel worksheet function name.
///
/// Registration creates a hidden defined name. Its grammar permits Unicode
/// characters as well as ASCII letters, `_`, and `\\`; later characters may
/// also be decimal digits, `.`, or `?`. Whitespace and control characters are
/// rejected. Cell references use the Excel 2007-and-later grid limits.
/// A1 rows use ASCII digits; R1C1 indices also recognize the Unicode 5.1
/// decimal digits specified by Excel's formula grammar.
/// Descriptions and argument labels have separate counted-string rules.
/// Excel remains the authority for registration acceptance and name conflicts.
///
/// See Microsoft's [formula name grammar] and [R1C1 reference grammar].
///
/// [formula name grammar]: https://learn.microsoft.com/en-us/openspecs/office_standards/ms-xlsx/3d025add-118d-4413-9856-ab65712ec1b0
/// [R1C1 reference grammar]: https://learn.microsoft.com/en-us/openspecs/office_standards/ms-oi29500/8d68c790-0571-44f6-8583-f837e72635ed
pub fn validate_function_name(name: &str) -> Result<(), FunctionNameError> {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err(FunctionNameError::Empty);
    };
    if name.encode_utf16().count() > EXCEL_FUNCTION_NAME_LIMIT {
        return Err(FunctionNameError::TooLong);
    }
    if !is_function_name_start(first) {
        return Err(FunctionNameError::InvalidStart);
    }
    if !characters.all(|character| {
        is_function_name_start(character)
            || character.is_ascii_digit()
            || matches!(character, '.' | '?')
    }) {
        return Err(FunctionNameError::InvalidCharacter);
    }
    if ["C", "R", "TRUE", "FALSE"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return Err(FunctionNameError::ReservedName);
    }
    if is_a1_reference(name) || is_r1c1_reference(name) {
        return Err(FunctionNameError::CellReference);
    }
    Ok(())
}

fn is_function_name_start(character: char) -> bool {
    character.is_ascii_alphabetic()
        || matches!(character, '_' | '\\')
        || (!character.is_ascii() && !character.is_whitespace() && !character.is_control())
}

const EXCEL_GRID_ROWS: u32 = 1_048_576;
const EXCEL_GRID_COLUMNS: u32 = 16_384;

fn is_a1_reference(name: &str) -> bool {
    let column_end = name.bytes().take_while(u8::is_ascii_alphabetic).count();
    if column_end == 0 || column_end > 3 {
        return false;
    }
    let (column_text, row_text) = name.split_at(column_end);
    let column = column_text.bytes().fold(0u32, |column, character| {
        column * 26 + u32::from(character.to_ascii_uppercase() - b'A' + 1)
    });
    column <= EXCEL_GRID_COLUMNS && row_text.is_ascii() && is_grid_index(row_text, EXCEL_GRID_ROWS)
}

fn is_r1c1_reference(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first().copied() else {
        return false;
    };
    let (first_limit, second_axis, second_limit) = match first.to_ascii_uppercase() {
        b'R' => (EXCEL_GRID_ROWS, b'C', EXCEL_GRID_COLUMNS),
        b'C' => (EXCEL_GRID_COLUMNS, b'R', EXCEL_GRID_ROWS),
        _ => return false,
    };
    let first_end = 1 + name[1..]
        .chars()
        .take_while(|character| unicode_decimal_digit(*character).is_some())
        .map(char::len_utf8)
        .sum::<usize>();
    let first_text = &name[1..first_end];
    if first_end == name.len() {
        return is_grid_index(first_text, first_limit);
    }
    if bytes[first_end].to_ascii_uppercase() != second_axis {
        return false;
    }
    let second_text = &name[first_end + 1..];
    // R1C1 permits omitted row/column indices for the current row/column.
    // The reversed C1R1 spelling requires both absolute indices.
    let relative_allowed = first.eq_ignore_ascii_case(&b'R');
    (is_grid_index(first_text, first_limit) || (relative_allowed && first_text.is_empty()))
        && (is_grid_index(second_text, second_limit)
            || (relative_allowed && second_text.is_empty()))
}

fn is_grid_index(text: &str, limit: u32) -> bool {
    let mut index = 0u32;
    for character in text.chars() {
        let Some(digit) = unicode_decimal_digit(character) else {
            return false;
        };
        let Some(next) = index
            .checked_mul(10)
            .and_then(|index| index.checked_add(digit))
            .filter(|index| *index <= limit)
        else {
            return false;
        };
        index = next;
    }
    index != 0
}

// Excel's formula grammar specifies Unicode 5.1 digits. Each Nd block below
// contains ten consecutive code points with decimal values zero through nine.
// Generated from records with General_Category=Nd and Decimal_Digit_Value=0:
// https://www.unicode.org/Public/5.1.0/ucd/UnicodeData.txt
const UNICODE_DECIMAL_ZEROES: [u32; 37] = [
    0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1B50, 0x1BB0,
    0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xAA50, 0xFF10, 0x104A0, 0x1D7CE, 0x1D7D8, 0x1D7E2,
    0x1D7EC, 0x1D7F6,
];

fn unicode_decimal_digit(character: char) -> Option<u32> {
    let codepoint = u32::from(character);
    UNICODE_DECIMAL_ZEROES.iter().find_map(|zero| {
        codepoint
            .checked_sub(*zero)
            .filter(|difference| *difference < 10)
    })
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
        .fold(0usize, usize::saturating_add)
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
    fn function_names_accept_excel_grammar_and_unicode() {
        for name in [
            "EXAMPLE.ADD",
            "TEST.CFG_ATTR.VALUE",
            "_private",
            "\\namespace",
            "A\\B",
            "Value?",
            "日本語.値２",
            "Cafe\u{301}",
            "計算💡",
            "１value",
            "R1C1.VALUE",
        ] {
            assert_eq!(validate_function_name(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn function_names_reject_invalid_characters_and_reserved_names() {
        assert_eq!(validate_function_name(""), Err(FunctionNameError::Empty));
        for name in ["1value", ".value", "?value", " value", "\0value"] {
            assert_eq!(
                validate_function_name(name),
                Err(FunctionNameError::InvalidStart),
                "{name:?}"
            );
        }
        for name in [
            "bad name",
            "bad-name",
            "bad+name",
            "bad,name",
            "bad\0name",
            "bad\rname",
            "bad\nname",
            "bad\u{a0}name",
            "r#type",
        ] {
            assert_eq!(
                validate_function_name(name),
                Err(FunctionNameError::InvalidCharacter),
                "{name:?}"
            );
        }
        for name in ["C", "c", "R", "r", "TRUE", "True", "FALSE", "false"] {
            assert_eq!(
                validate_function_name(name),
                Err(FunctionNameError::ReservedName),
                "{name:?}"
            );
        }
    }

    #[test]
    fn function_names_reject_only_references_within_the_excel_grid() {
        for name in [
            "A1",
            "a1",
            "XFD1048576",
            "xfd1048576",
            "R1C1",
            "C1R1",
            "r1048576c16384",
            "C16384R1048576",
            "RC",
            "R1C",
            "RC1",
        ] {
            assert_eq!(
                validate_function_name(name),
                Err(FunctionNameError::CellReference),
                "{name:?}"
            );
        }
        for name in [
            "A0",
            "A1048577",
            "XFE1",
            "AAAA1",
            "R0C1",
            "R1C0",
            "R1048577C1",
            "R1C16385",
            "C16385R1",
            "R1C1VALUE",
            "CR",
            "R999999999999999999999999999999999C1",
        ] {
            assert_eq!(validate_function_name(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn function_name_limit_counts_utf16_units() {
        let ascii_maximum = "x".repeat(EXCEL_FUNCTION_NAME_LIMIT);
        assert_eq!(validate_function_name(&ascii_maximum), Ok(()));
        assert_eq!(
            validate_function_name(&(ascii_maximum + "x")),
            Err(FunctionNameError::TooLong)
        );
        let unicode_maximum = format!("x{}", "💡".repeat(127));
        assert_eq!(validate_function_name(&unicode_maximum), Ok(()));
        assert_eq!(
            validate_function_name(&(unicode_maximum + "x")),
            Err(FunctionNameError::TooLong)
        );
        assert_eq!(validate_excel_string(&"x".repeat(256)), Ok(()));
    }

    #[test]
    fn r1c1_references_recognize_unicode_digits_without_widening_a1() {
        for name in [
            "R１C１",
            "C１R１",
            "R١C١",
            "R١",
            "C١",
            "R१०C१२",
            "R１０４８５７６C１６３８４",
            "C１６３８４R１０４８５７６",
        ] {
            assert_eq!(
                validate_function_name(name),
                Err(FunctionNameError::CellReference),
                "{name:?}"
            );
        }
        for name in [
            "A１",
            "Ｒ１Ｃ１",
            "R０C１",
            "R１０４８５７７C１",
            "R１C１６３８５",
            "C１６３８５R１",
            "R１C１.VALUE",
            "R９９９９９９９９９９９９９９９９９９９９C１",
        ] {
            assert_eq!(validate_function_name(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn unicode_decimal_digits_match_all_unicode_5_1_blocks() {
        for zero in UNICODE_DECIMAL_ZEROES {
            for digit in 0..10 {
                assert_eq!(
                    unicode_decimal_digit(char::from_u32(zero + digit).unwrap()),
                    Some(digit)
                );
            }
        }
        for character in ['x', '💡', '²', '\u{1e950}'] {
            assert_eq!(unicode_decimal_digit(character), None);
        }
    }

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
