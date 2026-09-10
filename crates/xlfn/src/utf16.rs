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
    let length = if text.is_ascii() {
        text.len()
    } else {
        // Valid UTF-8 contributes one UTF-16 unit per leading byte, plus
        // one more for each four-byte scalar (a surrogate pair). Counting
        // independent bytes can vectorize and avoids decoding characters
        // merely to determine the allocation size.
        text.as_bytes()
            .iter()
            .map(|&byte| usize::from(byte & 0xc0 != 0x80) + usize::from(byte >= 0xf0))
            .sum()
    };
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

/// Validates that `text` does not exceed `limit` UTF-16 code units.
///
/// This is a validation-only operation that does not return the code-unit count.
/// Because every valid UTF-8 byte corresponds to at most one UTF-16 code unit
/// (ASCII = 1 byte/unit, BMP = 2..3 bytes/unit, astral = 4 bytes/2 units),
/// `text.len() <= limit` guarantees that the UTF-16 code unit count cannot exceed `limit`.
///
/// Longer UTF-8 strings are counted once, including the exact length used
/// in an error, without materializing encoded units.
pub(crate) fn validate_utf16_limit(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<()> {
    // Fast path: UTF-16 code-unit count <= UTF-8 byte count for every valid Rust str.
    if text.len() <= limit {
        return Ok(());
    }

    checked_utf16_len(text, argument, limit).map(|_| ())
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

    proptest::proptest! {
        #[test]
        fn counted_length_matches_standard_unicode_encoding(text in ".{0,256}") {
            proptest::prop_assert_eq!(
                checked_utf16_len(&text, "test", usize::MAX).unwrap(),
                text.encode_utf16().count(),
            );
        }
    }

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

    #[test]
    fn validate_utf16_limit_ascii_boundaries() {
        let ascii_at_limit = "x".repeat(EXCEL_STRING_LIMIT);
        assert_eq!(ascii_at_limit.len(), 32_767);
        assert!(validate_utf16_limit(&ascii_at_limit, "test", EXCEL_STRING_LIMIT).is_ok());

        let ascii_over_limit = "x".repeat(EXCEL_STRING_LIMIT + 1);
        assert_eq!(ascii_over_limit.len(), 32_768);
        assert!(matches!(
            validate_utf16_limit(&ascii_over_limit, "test", EXCEL_STRING_LIMIT),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 32_767,
                    actual: 32_768
                },
                ..
            })
        ));
    }

    #[test]
    fn validate_utf16_limit_multibyte_bmp() {
        // "あ" is 3 bytes in UTF-8, but exactly 1 code unit in UTF-16.
        let bmp_at_limit = "あ".repeat(EXCEL_STRING_LIMIT);
        assert_eq!(bmp_at_limit.len(), 32_767 * 3);
        assert!(validate_utf16_limit(&bmp_at_limit, "test", EXCEL_STRING_LIMIT).is_ok());

        let bmp_over_limit = "あ".repeat(EXCEL_STRING_LIMIT + 1);
        assert!(matches!(
            validate_utf16_limit(&bmp_over_limit, "test", EXCEL_STRING_LIMIT),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 32_767,
                    actual: 32_768
                },
                ..
            })
        ));
    }

    #[test]
    fn validate_utf16_limit_astral_surrogate_pairs() {
        // "😀" is 4 bytes in UTF-8 and 2 code units (surrogate pair) in UTF-16.
        let astral_at_32766 = "😀".repeat(16_383);
        assert_eq!(astral_at_32766.len(), 16_383 * 4);
        assert!(validate_utf16_limit(&astral_at_32766, "test", EXCEL_STRING_LIMIT).is_ok());

        let astral_over_at_32768 = "😀".repeat(16_384);
        assert_eq!(astral_over_at_32768.len(), 16_384 * 4);
        assert!(matches!(
            validate_utf16_limit(&astral_over_at_32768, "test", EXCEL_STRING_LIMIT),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 32_767,
                    actual: 32_768
                },
                ..
            })
        ));
    }

    #[test]
    fn validate_utf16_limit_mixed_boundary() {
        // 32,764 ASCII bytes + "あ" (1 code unit) + "😀" (2 code units) = 32,767 code units
        let mut mixed_at_limit = "a".repeat(32_764);
        mixed_at_limit.push('あ');
        mixed_at_limit.push('😀');
        assert!(validate_utf16_limit(&mixed_at_limit, "test", EXCEL_STRING_LIMIT).is_ok());

        // 32,765 ASCII bytes + "あ" (1 code unit) + "😀" (2 code units) = 32,768 code units
        let mut mixed_over_limit = "a".repeat(32_765);
        mixed_over_limit.push('あ');
        mixed_over_limit.push('😀');
        assert!(matches!(
            validate_utf16_limit(&mixed_over_limit, "test", EXCEL_STRING_LIMIT),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 32_767,
                    actual: 32_768
                },
                ..
            })
        ));
    }
}
