// module: registration/host
use super::{DeleteName, RegistrationInvocation, Unregister};

fn expects_unregister(_: RegistrationInvocation<Unregister>) {}

fn substitute_same_output_type(result: RegistrationInvocation<DeleteName>) {
    expects_unregister(result); // error: E0308
}
