// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<A: crate::Addin>(owner: RemovalOwner<'_, A, FinalRemoval>, other: ShutdownDeps<'_, A>) {
    let _ = RemovalOwner::<A, FinalRemoval>::from_claim(other, owner); // error: E0599
}
