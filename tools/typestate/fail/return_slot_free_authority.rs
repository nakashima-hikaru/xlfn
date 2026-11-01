// module: return_abi
use super::slot::ReturnBlockSlot;
fn invalid(slot: &ReturnBlockSlot, pointer: *mut super::ReturnBlock) {
    let _claim = slot.claim_free(pointer); // error: E0133
}
