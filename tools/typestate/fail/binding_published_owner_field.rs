// module: handle
use super::binding::BindingSlot;

fn invalid(slot: BindingSlot) {
    let owner = slot.record.unwrap();
    let _raw = owner.owner; // error: E0616
}
