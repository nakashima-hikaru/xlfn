// module: return_abi
use super::slot::FreeingSlot;
fn publish(prepared: super::PreparedPublication) {
    super::RETURN_BLOCK_SLOT.with(|slot| {
        if let Some(reserved) = slot.try_reserve() {
            let initialized = reserved.initialize(prepared.into_block());
            let pointer = initialized.publish().into_pointer();
            // SAFETY: this local caller finishes reading its exact publication.
            unsafe { super::free_return(pointer) };
        }
    });
}
fn finish(freeing: FreeingSlot<'_>) {
    if let Ok(reusable) = freeing.destroy() { reusable.release(); }
}
