// module: kernel/lib
use crate::thread_affine::{ThreadAffineSlot};
fn invalid(slot: &ThreadAffineSlot<usize>) {
    let _ = slot.install_claimed(1); // error: E0599
    let _ = slot.take_claimed(); // error: E0599
    let _ = slot.release_empty_claimed(); // error: E0599
}
