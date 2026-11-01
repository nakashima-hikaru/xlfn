// module: return_abi
use super::slot::ReservedSlot;
fn invalid(reserved: ReservedSlot<'_>, first: super::ReturnBlock, second: super::ReturnBlock) {
    let _initialized = reserved.initialize(first);
    let _other = reserved.initialize(second); // error: E0382
}
