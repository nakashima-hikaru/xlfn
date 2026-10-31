// module: kernel/rotating_read_domain
use super::RetiredQueue;
fn invalid(retired: RetiredQueue<'_, 1, usize>) {
    retired.take(|_| (), ()); // error: E0061
}
