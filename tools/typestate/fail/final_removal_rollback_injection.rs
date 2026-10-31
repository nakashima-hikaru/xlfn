// module: runtime/shutdown/pipeline
use crate::runtime::shutdown::{FinalRemoval, RemovalOwner};
use crate::runtime::capabilities::RollbackRequest;
fn invalid<'a, A: crate::Addin>(owner: RemovalOwner<'a, A, FinalRemoval>, request: RollbackRequest<'a, A>) {
    let _ = owner.drain_opening(request); // error: E0599
}
