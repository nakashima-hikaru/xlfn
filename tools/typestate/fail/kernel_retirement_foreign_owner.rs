// module: kernel/rotating_read_domain
use super::{RetiredQueue, RotatingRetirementDomain};
fn invalid<'a>(mut retired: RetiredQueue<'a, 1, usize>, other: &'a RotatingRetirementDomain<1, usize>) {
    retired.owner = other; // error: E0616
}
