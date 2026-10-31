// module: kernel/lib
use crate::thread_affine::{ThreadAffineSlot, ThreadAffineAccess};
fn invalid(slot: &ThreadAffineSlot<usize>, access: &ThreadAffineAccess<'_, usize>) {
    let _ = slot.install(access, 1); // error: E0624
    let _ = slot.take(access); // error: E0624
    let _ = slot.release_empty_binding(access); // error: E0624
}
