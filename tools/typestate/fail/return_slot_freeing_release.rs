// module: return_abi
use super::slot::FreeingSlot;
fn invalid(freeing: FreeingSlot<'_>) {
    freeing.release(); // error: E0599
}
