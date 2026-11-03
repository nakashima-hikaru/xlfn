// module: kernel/service_slot/transaction
use super::reclamation::WithdrawnServiceOwner;
fn promote(owner: WithdrawnServiceOwner<u32>) {
    let _reclaimed = owner.after_drain(); // error: E0599
}
