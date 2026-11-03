// module: kernel/rotating_read_domain
use super::transition::PendingRotation;
fn invalid(pending: PendingRotation<'_, 1>) {
    pending.run(|_| ()); // error: E0599
}
