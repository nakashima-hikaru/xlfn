// module: kernel/quota
use super::Quota;
fn invalid(quota: &Quota) {
    let _permit = quota.try_acquire_detached(); // error: E0133
}
