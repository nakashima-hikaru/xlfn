// module: runtime_components
use crate::registration::{SuccessfulRegistrations, RegistrationId};
fn invalid(ids: Vec<RegistrationId>) {
    let _ = SuccessfulRegistrations::from_applied(ids); // error: E0624
}
