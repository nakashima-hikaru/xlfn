// module: runtime
use super::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<A: crate::Addin>(owner: RemovalOwner<'_, A, FinalRemoval>) {
    let _first = owner.drain();
    let _second = owner.drain(); // error: E0382
}
