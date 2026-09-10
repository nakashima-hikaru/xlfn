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
}
