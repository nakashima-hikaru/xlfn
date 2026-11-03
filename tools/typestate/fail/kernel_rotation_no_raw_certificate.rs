// module: kernel/rotating_read_domain
use super::{GenerationIndex, RotatingReadDomain};
fn invalid(domain: &RotatingReadDomain<1>) {
    let mut guard = domain.transition.lock();
    let _ = domain.drained_generation(GenerationIndex(0), &mut guard); // error: E0599
}
