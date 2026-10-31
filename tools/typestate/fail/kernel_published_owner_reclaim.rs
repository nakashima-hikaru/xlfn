// module: kernel/service_slot
use super::transaction::PublishedServiceOwner;
fn reclaim(owner: PublishedServiceOwner<u32>) {
    let _box = owner.into_box(); // error: E0599
}
