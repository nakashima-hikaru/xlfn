// module: kernel/rotating_read_domain
use super::{DrainedGeneration, RotatingRetirementDomain};
fn invalid(proof: DrainedGeneration<'_>, other: &RotatingRetirementDomain<1, Vec<usize>>) {
    let _ = proof.take_queue(&other.domain, &other.queues, drop); // error: E0599
}
