// module: kernel/service_slot/transaction
use super::reclamation::WithdrawnServiceOwner;
fn recover(owner: WithdrawnServiceOwner<u32>) {
    let _raw_owner = owner.0; // error: E0616
}
