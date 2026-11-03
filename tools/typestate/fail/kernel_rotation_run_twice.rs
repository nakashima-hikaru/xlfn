// module: kernel/rotating_read_domain
use super::transition::DrainedRotation;
fn invalid(drained: DrainedRotation<'_, 1>) {
    let _first = drained.run(|_| ());
    let _second = drained.run(|_| ()); // error: E0382
}
