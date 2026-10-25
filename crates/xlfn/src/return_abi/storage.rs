use std::panic::AssertUnwindSafe;

use crate::error::InputError;
use crate::{XllError, XllResult};
use xlfn_kernel::published_owner::PublishedOwner;

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

/// Owns one exactly sized counted scalar string. Its pointer remains valid
/// while this owner moves through scalar conversion and return publication.
#[derive(Debug)]
pub(crate) struct ScalarStringStorage {
    units: PublishedOwner<[u16]>,
}

impl ScalarStringStorage {
    /// Allocates exactly the counted scalar payload, before publishing a
    /// pointer through the non-retagging owner.
    #[allow(
        unsafe_code,
        reason = "initializes every counted UTF-16 unit before publishing its owner"
    )]
    pub(crate) fn new(
        text: &str,
        argument: &'static str,
        limit: usize,
        length: usize,
    ) -> XllResult<(Self, *mut u16)> {
        let length = u16::try_from(length).map_err(|_| {
            XllError::input(
                argument,
                InputError::TooLarge {
                    limit,
                    actual: length,
                },
            )
        })?;
        let mut units = Box::<[u16]>::new_uninit_slice(usize::from(length) + 1);
        units[0].write(length);
        let payload = &mut units[1..];
        if text.len() == usize::from(length) {
            // Equality also proves equal loop lengths independently of the
            // supplied Unicode count: zip initializes every payload unit.
            for (unit, &byte) in payload.iter_mut().zip(text.as_bytes()) {
                unit.write(u16::from(byte));
            }
        } else if text.len() == usize::from(length) * 3 {
            // The u16-bound count cannot overflow usize when tripled. Exact
            // byte equality gives exactly one complete chunk per destination,
            // even if an internal caller supplied an incorrect Unicode count.
            for (unit, &[first, second, third]) in
                payload.iter_mut().zip(text.as_bytes().as_chunks::<3>().0)
            {
                unit.write(
                    (u16::from(first & 0x0f) << 12)
                        | (u16::from(second & 0x3f) << 6)
                        | u16::from(third & 0x3f),
                );
            }
        } else {
            let mut encoded = text.encode_utf16();
            for unit in payload {
                unit.write(
                    encoded
                        .next()
                        .expect("the UTF-16 length was counted before allocation"),
                );
            }
            debug_assert!(encoded.next().is_none());
        }
        // SAFETY: the prefix was initialized first. Each fixed-width branch
        // proves equal source/destination lengths; the general branch writes
        // every destination or panics before reaching this point. Thus full
        // initialization does not depend on the caller's Unicode count being
        // correct (that count is needed for the string's semantic accuracy).
        let owner = PublishedOwner::from_box(unsafe { units.assume_init() });
        let pointer = owner.as_ptr().cast::<u16>();
        Ok((Self { units: owner }, pointer))
    }

    #[cfg(test)]
    pub(crate) fn allocated_bytes(&self) -> usize {
        std::mem::size_of_val(self.units.as_ref())
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
            proptest::prop_assert_eq!(&actual[1..], &expected);

            let (scalar, pointer) = ScalarStringStorage::new(
                &text,
                "test",
                crate::utf16::EXCEL_STRING_LIMIT,
                expected.len(),
            ).unwrap();
            // SAFETY: scalar retains every initialized unit through the read.
            let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
            proptest::prop_assert_eq!(usize::from(actual[0]), expected.len());
            proptest::prop_assert_eq!(&actual[1..], expected);
            proptest::prop_assert_eq!(scalar.allocated_bytes(), 2 * actual.len());
        }
    }

    #[test]
    fn miri_scalar_payload_pointer_survives_owner_moves() {
        for text in ["", "a\0z", "日", "aé日💡", &"日💡".repeat(100)] {
            let expected: Vec<_> = text.encode_utf16().collect();
            let (storage, pointer) = ScalarStringStorage::new(
                text,
                "test",
                crate::utf16::EXCEL_STRING_LIMIT,
                expected.len(),
            )
            .unwrap();
            let mut owners = vec![storage];
            for _ in 0..64 {
                owners.push(
                    ScalarStringStorage::new("", "test", crate::utf16::EXCEL_STRING_LIMIT, 0)
                        .unwrap()
                        .0,
                );
            }
            let original = owners.remove(0);
            owners.push(original);
            // SAFETY: the last owner retains the counted payload after every
            // move; the published raw pointer does not borrow owner storage.
            let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
            assert_eq!(usize::from(actual[0]), expected.len());
            assert_eq!(&actual[1..], expected);
            assert_eq!(owners.last().unwrap().allocated_bytes(), 2 * actual.len());
        }
    }

    #[test]
    fn miri_scalar_initialization_does_not_trust_the_supplied_count() {
        for text in ["", "abc", "é", "日", "aé日💡"] {
            for length in 0..=text.len() + 1 {
                let result = crate::panic_boundary::catch_no_unwind(|| {
                    ScalarStringStorage::new(text, "test", crate::utf16::EXCEL_STRING_LIMIT, length)
                });
                // A malformed internal count may panic or encode incorrect
                // text; it must never expose an uninitialized allocation.
                if let Ok(Ok((owner, pointer))) = result {
                    // SAFETY: a successful safe constructor promises all
                    // units are initialized, even for these negative controls.
                    let units = unsafe { std::slice::from_raw_parts(pointer, length + 1) };
                    assert_eq!(usize::from(units[0]), length);
                    let checksum: usize = units.iter().map(|&unit| usize::from(unit)).sum();
                    assert!(checksum <= (length + 1) * usize::from(u16::MAX));
                    drop(owner);
                }
            }
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

            let (scalar, pointer) = ScalarStringStorage::new(
                text,
                "test",
                crate::utf16::EXCEL_STRING_LIMIT,
                expected.len(),
            )
            .unwrap();
            // SAFETY: this scalar owner retains the initialized allocation.
            let actual = unsafe { std::slice::from_raw_parts(pointer, expected.len() + 1) };
            assert_eq!(usize::from(actual[0]), expected.len());
            assert_eq!(&actual[1..], expected);
            assert_eq!(scalar.allocated_bytes(), 2 * actual.len());
        }
    }
}
