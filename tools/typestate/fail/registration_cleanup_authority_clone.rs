// module: registration
use super::PendingRegistration;
fn invalid(entry: PendingRegistration) {
    let _duplicate = entry.clone(); // error: E0599
}
