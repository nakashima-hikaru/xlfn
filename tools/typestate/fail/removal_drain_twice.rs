// module: runtime
use super::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<A: crate::Addin>(owner: RemovalOwner<'_, A>) {
    let _first = owner.drain::<FinalRemoval>();
    let _second = owner.drain::<FinalRemoval>(); // error: E0382
}
