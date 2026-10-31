// module: kernel/rotating_read_domain
use super::transition::DrainedRotation;
use super::GenerationIndex;
fn invalid(mut drained: DrainedRotation<'_, 1>) {
    drained.old = GenerationIndex(0); // error: E0616
}
