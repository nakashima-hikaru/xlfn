// module: kernel/quota
use super::{Quota, QuotaPermit};
fn invalid() -> QuotaPermit<'static> {
    let quota = Quota::new(1);
    quota.try_acquire().unwrap() // error: E0515
}
