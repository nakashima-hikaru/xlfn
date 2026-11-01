// module: return_abi
use super::slot::InitializedSlot;
fn invalid(initialized: InitializedSlot<'_>) {
    let _first = initialized.publish();
    let _second = initialized.publish(); // error: E0382
}
