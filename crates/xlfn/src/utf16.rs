use crate::error::InputError;
use crate::{XllError, XllResult};
use smallvec::SmallVec;
use std::mem::MaybeUninit;

pub(crate) use xlfn_common::EXCEL_STRING_LIMIT;
const INLINE_UTF16_CAPACITY: usize = 64;

/// Plans one strict UTF-16 conversion without allocating intermediate text.
/// Valid input has an exact UTF-8 length. A lone surrogate contributes two
/// bytes to the capacity bound but is rejected by the decoder before output
/// is published.
pub(crate) struct Utf16Decoder<'input> {
    units: &'input [u16],
    utf8_len: usize,
}

impl<'input> Utf16Decoder<'input> {
    #[inline]
    pub(crate) fn new(units: &'input [u16]) -> Self {
        // A valid surrogate pair contributes two bytes per unit, four total.
        // Independent unit classification lets the compiler vectorize this
        // reduction and also proves ASCII when both lengths are equal.
        let utf8_len = units
            .iter()
            .map(|&unit| {
                1 + usize::from(unit >= 0x80) + usize::from(unit >= 0x800)
                    - usize::from((0xd800..=0xdfff).contains(&unit))
            })
            .sum();
        Self { units, utf8_len }
    }

    pub(crate) fn utf8_len(&self) -> usize {
        self.utf8_len
    }

    #[allow(
        unsafe_code,
        reason = "publishes only the initialized UTF-8 prefix produced by a strict Unicode decoder"
    )]
    #[inline]
    pub(crate) fn decode_into<'output>(
        &self,
        output: &'output mut [MaybeUninit<u8>],
        argument: &'static str,
    ) -> XllResult<&'output str> {
        let output = &mut output[..self.utf8_len];
        let initialized = if self.utf8_len == self.units.len() {
            for (byte, &unit) in output.iter_mut().zip(self.units) {
                byte.write(unit as u8);
            }
            self.utf8_len
        } else {
            let mut initialized = 0;
            let pointer = output.as_mut_ptr().cast::<u8>();
            for character in char::decode_utf16(self.units.iter().copied()) {
                let scalar = character
                    .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))?
                    as u32;
                let target = pointer.wrapping_add(initialized);
                let length = match scalar {
                    0..=0x7f => {
                        // SAFETY: the planned capacity includes this valid
                        // scalar; target is its next uninitialized byte.
                        unsafe { target.write(scalar as u8) };
                        1
                    }
                    0x80..=0x7ff => {
                        let bytes = [(0xc0 | (scalar >> 6)) as u8, (0x80 | (scalar & 0x3f)) as u8];
                        // SAFETY: the plan reserves two bytes for this scalar.
                        // [u8; 2] has alignment one and the whole slot is live.
                        unsafe { target.cast::<[u8; 2]>().write(bytes) };
                        2
                    }
                    0x800..=0xffff => {
                        let bytes = [
                            (0xe0 | (scalar >> 12)) as u8,
                            (0x80 | ((scalar >> 6) & 0x3f)) as u8,
                            (0x80 | (scalar & 0x3f)) as u8,
                        ];
                        // SAFETY: the plan reserves three bytes for this scalar.
                        // [u8; 3] has alignment one and the whole slot is live.
                        unsafe { target.cast::<[u8; 3]>().write(bytes) };
                        3
                    }
                    _ => {
                        let bytes = [
                            (0xf0 | (scalar >> 18)) as u8,
                            (0x80 | ((scalar >> 12) & 0x3f)) as u8,
                            (0x80 | ((scalar >> 6) & 0x3f)) as u8,
                            (0x80 | (scalar & 0x3f)) as u8,
                        ];
                        // SAFETY: a valid surrogate pair contributes four
                        // bytes to the plan; the byte array has alignment one.
                        unsafe { target.cast::<[u8; 4]>().write(bytes) };
                        4
                    }
                };
                initialized += length;
            }
            initialized
        };
        // SAFETY: each byte in this prefix was initialized above, either by
        // ASCII narrowing or by encoding validated Unicode scalar values.
        let bytes = unsafe { output[..initialized].assume_init_ref() };
        // SAFETY: both initialization paths produce valid UTF-8, and lone
        // surrogates return before the prefix can be exposed.
        Ok(unsafe { std::str::from_utf8_unchecked(bytes) })
    }
}

#[allow(
    unsafe_code,
    reason = "transfers the initialized UTF-8 prefix from the shared strict decoder into String ownership"
)]
pub(crate) fn decode_owned(units: &[u16], argument: &'static str) -> XllResult<String> {
    let decoder = Utf16Decoder::new(units);
    let mut bytes = Vec::with_capacity(decoder.utf8_len());
    let initialized = decoder
        .decode_into(bytes.spare_capacity_mut(), argument)?
        .len();
    // SAFETY: decode_into initialized this prefix within the allocated capacity.
    unsafe { bytes.set_len(initialized) };
    // SAFETY: decode_into returned a valid str over exactly this prefix.
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

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

#[allow(
    unsafe_code,
    reason = "initializes checked-capacity UTF-16 storage before publishing its length"
)]
pub(crate) fn encode_counted(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<SmallVec<[u16; INLINE_UTF16_CAPACITY]>> {
    let limit = limit.min(usize::from(u16::MAX));
    let (capacity, ascii) = if text.len() < INLINE_UTF16_CAPACITY && text.len() <= limit {
        // UTF-16 length never exceeds UTF-8 length. A short input already
        // proves both limits without decoding Unicode just to count it.
        (text.len() + 1, text.is_ascii())
    } else {
        let length = checked_utf16_len(text, argument, limit)?;
        (length + 1, text.len() == length)
    };
    // Reserve by code-unit length, so Unicode text that fits inline does not
    // spill merely because its UTF-8 representation is larger. Long strings
    // get their final buffer immediately; rejected strings allocate nothing.
    let mut units = SmallVec::<[u16; INLINE_UTF16_CAPACITY]>::with_capacity(capacity);
    if ascii {
        units.push(text.len() as u16);
        units.extend(text.as_bytes().iter().map(|&byte| u16::from(byte)));
    } else {
        let pointer = units.as_mut_ptr();
        let mut initialized = 1;
        for unit in text.encode_utf16() {
            // SAFETY: capacity is either the exact UTF-16 length + 1 or a
            // proven UTF-8 upper bound. The encoder cannot exceed it, and
            // this owner is not moved or reallocated during initialization.
            unsafe { pointer.wrapping_add(initialized).write(unit) };
            initialized += 1;
        }
        // SAFETY: every payload unit is initialized above, the count fits
        // u16 after validation, and length is published only after the prefix
        // is initialized. No external references have been created yet.
        unsafe { pointer.write((initialized - 1) as u16) };
        // SAFETY: the prefix and all payload units are initialized, and the
        // final length is within the capacity bound established above.
        unsafe { units.set_len(initialized) };
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn owned_decode_matches_standard_strict_utf16(
            units in proptest::collection::vec(proptest::prelude::any::<u16>(), 0..512)
        ) {
            let expected = String::from_utf16(&units);
            let actual = decode_owned(&units, "text");
            match expected {
                Ok(expected) => {
                    proptest::prop_assert_eq!(Utf16Decoder::new(&units).utf8_len(), expected.len());
                    proptest::prop_assert_eq!(actual.unwrap(), expected);
                }
                Err(_) => proptest::prop_assert!(matches!(
                    actual,
                    Err(XllError::Input { argument: "text", reason: InputError::InvalidUtf16 }),
                ), "invalid UTF-16 must retain its typed input error"),
            }
        }

        #[test]
        fn counted_length_matches_standard_unicode_encoding(text in ".{0,256}") {
            proptest::prop_assert_eq!(
                checked_utf16_len(&text, "test", usize::MAX).unwrap(),
                text.encode_utf16().count(),
            );
        }

        #[test]
        fn counted_encoding_matches_standard_unicode_encoding(text in ".{0,512}") {
            let expected = text.encode_utf16().collect::<Vec<_>>();
            let actual = encode_counted(&text, "test", EXCEL_STRING_LIMIT).unwrap();
            proptest::prop_assert_eq!(usize::from(actual[0]), expected.len());
            proptest::prop_assert_eq!(&actual[1..], expected);
            proptest::prop_assert_eq!(decode_owned(&actual[1..], "test").unwrap(), text);
        }
    }

    #[test]
    fn miri_utf16_decoder_publishes_only_initialized_valid_utf8() {
        for units in [
            vec![],
            vec![0, 0x41, 0x7f],
            vec![0x80, 0x7ff, 0x800, 0xffff],
            vec![0xd800, 0xdc00, 0xdbff, 0xdfff],
            vec![0x41, 0xd800],
            vec![0xdc00, 0x41],
            vec![0xd800, 0x41],
        ] {
            let decoder = Utf16Decoder::new(&units);
            let mut storage = vec![MaybeUninit::uninit(); decoder.utf8_len() + 16];
            let decoded = decoder.decode_into(&mut storage, "text");
            match String::from_utf16(&units) {
                Ok(expected) => {
                    assert_eq!(decoded.unwrap(), expected);
                    assert_eq!(decode_owned(&units, "text").unwrap(), expected);
                }
                Err(_) => {
                    assert!(decoded.is_err());
                    assert!(decode_owned(&units, "text").is_err());
                }
            }
        }
    }

    #[test]
    fn miri_counted_unicode_encoding_crosses_inline_and_spilled_boundaries() {
        for repetitions in [0, 1, 7, 12, 13, 63, 64, 65] {
            let text = "aé日💡".repeat(repetitions);
            let expected = text.encode_utf16().collect::<Vec<_>>();
            let actual = encode_counted(&text, "test", EXCEL_STRING_LIMIT).unwrap();
            assert_eq!(usize::from(actual[0]), expected.len());
            assert_eq!(&actual[1..], expected);
        }
    }

    #[test]
    fn counted_encoding_preserves_length_limits() {
        for text in ["x".repeat(32_767), "あ".repeat(32_767), "💡".repeat(16_383)] {
            let encoded = encode_counted(&text, "test", EXCEL_STRING_LIMIT).unwrap();
            assert_eq!(usize::from(encoded[0]), text.encode_utf16().count());
            assert_eq!(encoded.len(), usize::from(encoded[0]) + 1);
        }
        for text in ["x".repeat(32_768), "あ".repeat(32_768), "💡".repeat(16_384)] {
            assert!(matches!(
                encode_counted(&text, "test", EXCEL_STRING_LIMIT),
                Err(XllError::Input {
                    argument: "test",
                    reason: InputError::TooLarge {
                        limit: 32_767,
                        actual: 32_768
                    },
                }),
            ));
        }
        assert!(matches!(
            encode_counted(&"x".repeat(65_536), "test", usize::MAX),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 65_535,
                    actual: 65_536
                },
                ..
            }),
        ));
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
