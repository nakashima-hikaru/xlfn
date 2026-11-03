// module: return_abi
use super::slot::InitializedSlot;
fn invalid(initialized: InitializedSlot<'_>) {
    let _ = initialized.into_pointer(); // error: E0599
}
