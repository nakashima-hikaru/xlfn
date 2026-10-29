// module: runtime
use super::{CanonicalCompletion, FinalRemoval, OpenRollback};

fn invalid<A: crate::Addin>(completion: CanonicalCompletion<'_, A, FinalRemoval>) {
    let _: CanonicalCompletion<'_, A, OpenRollback> = completion; // error: E0308
}
