// module: return_abi
fn invalid() {
    super::RETURN_BLOCK_SLOT.with(|slot| {
        let _ = slot.try_acquire(); // error: E0599
    });
}
