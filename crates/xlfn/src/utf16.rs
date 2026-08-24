use crate::error::InputError;
use crate::{XllError, XllResult};
use smallvec::SmallVec;

pub(crate) use xlfn_common::EXCEL_STRING_LIMIT;
const INLINE_UTF16_CAPACITY: usize = 64;

pub(crate) fn checked_utf16_len(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<usize> {
    let length = text.encode_utf16().count();
    if length > limit {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit,
                actual: length,
            },
        ));
    }
    Ok(length)
}

/// Compares UTF-16 code units using the same ASCII-only folding as
/// `str::eq_ignore_ascii_case`, without first allocating a UTF-8 `String`.
#[doc(hidden)]
#[inline]
pub fn utf16_eq_ignore_ascii_case(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(&left, &right)| fold_ascii(left) == fold_ascii(right))
}

#[inline]
const fn fold_ascii(unit: u16) -> u16 {
    match unit {
        0x41..=0x5a => unit + (0x61 - 0x41),
        _ => unit,
    }
}

pub(crate) fn encode_bounded(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<SmallVec<[u16; INLINE_UTF16_CAPACITY]>> {
    let mut units = SmallVec::new();
    let mut length = 0_usize;
    for unit in text.encode_utf16() {
        length += 1;
        if length <= limit {
            units.push(unit);
        }
    }
    if length > limit {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit,
                actual: length,
            },
        ));
    }
    Ok(units)
}

pub(crate) fn encode_counted(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<SmallVec<[u16; INLINE_UTF16_CAPACITY]>> {
    let mut units = SmallVec::new();
    units.push(0);
    let mut length = 0_usize;
    for unit in text.encode_utf16() {
        length += 1;
        if length <= limit {
            units.push(unit);
        }
    }
    if length > limit {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit,
                actual: length,
            },
        ));
    }
    units[0] = length as u16;
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_encoding_uses_one_final_buffer() {
        assert_eq!(
            encode_counted("価格", "test", EXCEL_STRING_LIMIT)
                .unwrap()
                .as_slice(),
            [2, 0x4fa1, 0x683c]
        );
    }

    #[test]
    fn counted_encoding_sizes_inline_storage_in_utf16_units() {
        let encoded = encode_counted(
            "日本語日本語日本語日本語日本語日本語日本語日本語",
            "test",
            32_767,
        )
        .unwrap();
        assert_eq!(encoded.len(), 25);
        assert!(!encoded.spilled());
    }

    #[test]
    fn bounded_encoding_reports_the_full_utf16_length() {
        assert!(matches!(
            encode_bounded("a😀", "test", 1),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1,
                    actual: 3
                },
                ..
            })
        ));
    }

    #[test]
    fn utf16_ascii_case_folding_matches_without_decoding() {
        assert!(utf16_eq_ignore_ascii_case(
            &[
                b'L' as u16,
                b'i' as u16,
                b'n' as u16,
                b'e' as u16,
                b'a' as u16,
                b'r' as u16
            ],
            &[
                b'l' as u16,
                b'i' as u16,
                b'n' as u16,
                b'e' as u16,
                b'a' as u16,
                b'r' as u16
            ],
        ));
        assert!(!utf16_eq_ignore_ascii_case(
            &[
                b'L' as u16,
                b'i' as u16,
                b'n' as u16,
                b'e' as u16,
                b'a' as u16,
                b'r' as u16
            ],
            &[b'l' as u16, b'o' as u16, b'g' as u16],
        ));
        assert!(utf16_eq_ignore_ascii_case(&[0x00e9], &[0x00e9]));
        assert!(!utf16_eq_ignore_ascii_case(&[0x00e9], &[0x00c9]));
    }
}
