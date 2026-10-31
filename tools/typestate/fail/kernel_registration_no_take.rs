// module: kernel/rotating_read_domain
use super::RetirementRegistration;
fn invalid(mut registration: RetirementRegistration<'_, Vec<usize>>) {
    registration.pop(); // error: E0599
}
