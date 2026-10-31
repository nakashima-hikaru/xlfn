// module: kernel/rotating_read_domain
use super::RotatingRetirementDomain;
fn invalid(domain: &RotatingRetirementDomain<1, Vec<usize>>) {
    domain.inspect_queue(0, |_: &Vec<usize>| ()); // error: E0599
}
