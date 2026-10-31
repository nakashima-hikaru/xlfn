// module: kernel/rotating_read_domain
use super::RetirementRegistration;
fn invalid(mut registration: RetirementRegistration<'_, Vec<usize>>) {
    let _: &mut Vec<usize> = &mut *registration; // error: E0614
}
