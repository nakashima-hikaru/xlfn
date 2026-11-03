// module: return_abi
use super::slot::{FreeingSlot, InitializedSlot, PublishedSlot, ReservedSlot, ReusableSlot, ReturnBlockSlot};
fn invalid(slot: &ReturnBlockSlot, reserved: ReservedSlot<'_>, initialized: InitializedSlot<'_>, published: PublishedSlot<'_>, freeing: FreeingSlot<'_>, reusable: ReusableSlot<'_>) {
    let _ = &slot.state; // error: E0616
    let _ = &slot.block; // error: E0616
    let _ = reserved.slot; // error: E0616
    let _ = initialized.slot; // error: E0616
    let _ = published.slot; // error: E0616
    let _ = freeing.slot; // error: E0616
    let _ = reusable.slot; // error: E0616
}
