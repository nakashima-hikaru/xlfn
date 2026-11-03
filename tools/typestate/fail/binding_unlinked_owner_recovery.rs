// module: handle/domain
use super::retirement::UnlinkedBindingOwner;

fn invalid(owner: UnlinkedBindingOwner) {
    let _ = owner.into_box(); // error: E0599
}
