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
