// module: return_abi
fn invalid(pointer: *mut super::ReturnBlock) {
    super::RETURN_BLOCK_SLOT.with(|slot| {
        // This deliberately omits destruction and a matching freeing claim.
        unsafe { slot.release(pointer) }; // error: E0599
    });
}
