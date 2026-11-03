// module: runtime
use super::shutdown::{FinalRemoval, OpenRollback, RemovalOwner};

fn invalid<A: crate::Addin>(owner: RemovalOwner<'_, A, OpenRollback>) {
    let _ = owner.drain::<FinalRemoval>(); // error: E0107
}
