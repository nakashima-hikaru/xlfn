// module: kernel/service_slot
use super::transaction::QuarantinedServiceOwner;
fn reclaim(owner: QuarantinedServiceOwner<u32>) {
    let _box = owner.into_box(); // error: E0599
}
