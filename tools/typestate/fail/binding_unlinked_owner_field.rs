// module: handle/domain
use super::retirement::UnlinkedBindingOwner;

fn invalid(owner: UnlinkedBindingOwner) {
    let _raw = owner.owner; // error: E0616
}
