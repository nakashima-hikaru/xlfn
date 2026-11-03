// module: kernel/rotating_read_domain
use super::transition::DrainedRotation;
fn invalid(mut drained: DrainedRotation<'_, 1>) {
    let _ = drained.certificate(); // error: E0624
}
