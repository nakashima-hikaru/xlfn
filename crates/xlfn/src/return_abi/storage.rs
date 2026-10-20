use std::panic::AssertUnwindSafe;

use crate::error::InputError;
use crate::{XllError, XllResult};

/// Allocation storage shared by one encoded Excel return value.
///
/// The arena only contains raw return payloads (`u16` UTF-16 units). Those
/// values do not require individual destructors, so releasing the storage as
/// one unit preserves the return block's ownership boundary.
#[derive(Debug)]
pub(crate) struct ReturnStorage {
    pub(crate) arena: AssertUnwindSafe<bumpalo::Bump>,
}

impl ReturnStorage {
    pub(crate) fn new() -> Self {
        Self {
            arena: AssertUnwindSafe(bumpalo::Bump::new()),
        }
    }

    pub(crate) fn alloc_counted_utf16_with_length(
        &self,
        text: &str,
        argument: &'static str,
        limit: usize,
        length: usize,
    ) -> XllResult<*mut u16> {
        let length = u16::try_from(length).map_err(|_| {
            XllError::input(
                argument,
                InputError::TooLarge {
                    limit,
                    actual: length,
                },
            )
        })?;
        // For valid UTF-8, byte length equals UTF-16 length exactly when the
        // string is ASCII. The caller already counted the units, so this
        // selects direct widening without another ASCII scan or decoder.
        if text.len() == usize::from(length) {
            let bytes = text.as_bytes();
            let units = self.arena.alloc_slice_fill_with(bytes.len() + 1, |index| {
                if index == 0 {
                    length
                } else {
                    u16::from(bytes[index - 1])
                }
            });
            return Ok(units.as_mut_ptr());
        }
        // UTF-8 uses at most three bytes per UTF-16 unit. Equality proves
        // every scalar is a three-byte BMP scalar (astral scalars use two
        // units for four bytes). Rust str validity already excludes surrogates.
        if text.len() == usize::from(length) * 3 {
            let bytes = text.as_bytes().as_chunks::<3>().0;
            let units = self.arena.alloc_slice_fill_with(bytes.len() + 1, |index| {
                if index == 0 {
                    length
                } else {
                    let [first, second, third] = bytes[index - 1];
                    (u16::from(first & 0x0f) << 12)
                        | (u16::from(second & 0x3f) << 6)
                        | u16::from(third & 0x3f)
                }
            });
            return Ok(units.as_mut_ptr());
        }
        let mut encoded = text.encode_utf16();
        let units = self
            .arena
            .alloc_slice_fill_with(length as usize + 1, |index| {
                if index == 0 {
                    length
                } else {
                    encoded
                        .next()
                        .expect("the UTF-16 length was counted before allocation")
                }
            });
        debug_assert!(encoded.next().is_none());
        Ok(units.as_mut_ptr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn counted_storage_matches_standard_utf16(text in ".{0,256}") {
            let expected = text.encode_utf16().collect::<Vec<_>>();
            let storage = ReturnStorage::new();
            let pointer = storage.alloc_counted_utf16_with_length(
                &text,
                "test",
                crate::utf16::EXCEL_STRING_LIMIT,
                expected.len(),
            ).unwrap();
            // SAFETY: storage owns this initialized allocation of length + 1
            // units and remains live for the whole comparison.
            let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
            proptest::prop_assert_eq!(usize::from(actual[0]), expected.len());
            proptest::prop_assert_eq!(&actual[1..], expected);
        }
    }
    #[test]
    fn miri_counted_storage_fixed_bmp_and_fallback_boundaries() {
        for repetitions in [0, 1, 21, 22, 63, 64, 65, 128] {
            for seed in ["\u{800}\u{d7ff}\u{e000}\u{ffff}", "aé日💡", "日a", "日💡"] {
                let text = seed.repeat(repetitions);
                let expected: Vec<_> = text.encode_utf16().collect();
                let storage = ReturnStorage::new();
                let pointer = storage
                    .alloc_counted_utf16_with_length(
                        &text,
                        "test",
                        crate::utf16::EXCEL_STRING_LIMIT,
                        expected.len(),
                    )
                    .unwrap();
                // SAFETY: this arena owns all initialized units for the read.
                let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
                assert_eq!(usize::from(actual[0]), expected.len());
                assert_eq!(&actual[1..], expected);
            }
        }
    }

    #[test]
    fn counted_storage_encodes_every_three_byte_bmp_scalar() {
        let text: String = (0x800..=0xffff).filter_map(char::from_u32).collect();
        // Keep each return within the Excel code-unit limit.
        for bytes in text.as_bytes().chunks(3 * 1024) {
            let text = std::str::from_utf8(bytes).unwrap();
            let expected: Vec<_> = text.encode_utf16().collect();
            let storage = ReturnStorage::new();
            let pointer = storage
                .alloc_counted_utf16_with_length(
                    text,
                    "test",
                    crate::utf16::EXCEL_STRING_LIMIT,
                    expected.len(),
                )
                .unwrap();
            // SAFETY: this arena owns the initialized counted allocation.
            let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
            assert_eq!(usize::from(actual[0]), expected.len());
            assert_eq!(&actual[1..], expected);
        }
    }
}
