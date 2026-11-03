// module: kernel/rotating_read_domain
use super::transition::PendingRotation;
fn invalid(pending: PendingRotation<'_, 1>) {
    let first = pending.wait();
    let second = pending.wait(); // error: E0382
    drop((first, second));
}
