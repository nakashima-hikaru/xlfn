// module: return_abi
use super::slot::ReservedSlot;
fn invalid(reserved: ReservedSlot<'_>) {
    let _ = reserved.into_pointer(); // error: E0599
}
