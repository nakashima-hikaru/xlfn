// module: kernel/quota
use super::Quota;
fn valid(quota: &Quota) {
    let permit = quota.try_acquire().unwrap();
    drop(permit);
}
