// module: return_abi
use super::slot::ReturnBlockSlot;
fn invalid() {
    let _stack_root = ReturnBlockSlot::new(); // error: E0624
}
