// module: kernel/quota
use super::Quota;
fn invalid(quota: Quota) {
    let permit = quota.try_acquire().unwrap();
    drop(quota); // error: E0505
    drop(permit);
}
