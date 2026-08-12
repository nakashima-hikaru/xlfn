/// Allocation storage shared by one encoded Excel return value.
///
/// The arena only contains raw return payloads (`u16` UTF-16 units). Those
/// values do not require individual destructors, so releasing the storage as
/// one unit preserves the return block's ownership boundary.
#[derive(Debug)]
pub(crate) struct ReturnStorage {
    pub(crate) arena: bumpalo::Bump,
}

impl ReturnStorage {
    pub(crate) fn new() -> Self {
        Self {
            arena: bumpalo::Bump::new(),
        }
    }

    pub(crate) fn alloc_u16_slice(&self, values: &[u16]) -> *mut u16 {
        self.arena.alloc_slice_copy(values).as_mut_ptr()
    }
}
