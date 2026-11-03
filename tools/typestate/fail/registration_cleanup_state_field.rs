// module: registration
use super::{PendingRegistration, RegistrationCleanupState, RegistrationId};
fn invalid(mut entry: PendingRegistration, other: RegistrationId) {
    entry.state = RegistrationCleanupState::NameDeleted; // error: E0616
    entry.registration = other; // error: E0616
}
