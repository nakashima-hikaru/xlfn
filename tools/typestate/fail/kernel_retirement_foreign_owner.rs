// module: kernel/rotating_read_domain
use super::{DisposableBatch, RotatingRetirementDomain};
fn invalid<'a>(mut retired: DisposableBatch<'a, 1, Vec<usize>>, other: &'a RotatingRetirementDomain<1, Vec<usize>>) {
    retired.owner = other; // error: E0616
}
