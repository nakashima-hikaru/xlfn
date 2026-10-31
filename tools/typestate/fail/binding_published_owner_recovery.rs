// module: handle
use super::binding::BindingSlot;

fn invalid(slot: BindingSlot) {
    let owner = slot.record.unwrap();
    let _ = owner.into_box(); // error: E0599
}
