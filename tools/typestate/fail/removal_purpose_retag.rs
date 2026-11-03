// module: runtime
use super::shutdown::{FinalRemoval, OpenRollback, RemovalOwner};

fn invalid<A: crate::Addin>(owner: RemovalOwner<'_, A, OpenRollback>) {
    let _: RemovalOwner<'_, A, FinalRemoval> = owner; // error: E0308
}
