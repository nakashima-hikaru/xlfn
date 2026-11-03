// module: kernel/rotating_read_domain
use super::DisposableBatch;
fn invalid(retired: DisposableBatch<'_, 1, Vec<usize>>) {
    retired.into_inner();
    retired.into_inner(); // error: E0382
}
