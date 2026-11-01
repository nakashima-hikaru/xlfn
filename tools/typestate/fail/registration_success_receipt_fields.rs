// module: runtime_components
use crate::registration::SuccessfulRegistrations;
fn invalid(receipt: SuccessfulRegistrations) {
    let _ids = receipt.0; // error: E0616
}
