// module: runtime
use crate::lifecycle::OpenStart;
use crate::runtime::{Begun, OpeningTxn};

fn handoff<'a, A: crate::Addin>(start: OpenStart<'a, A>, successor: OpeningTxn<'a, A, Begun<A>>) {
    match start.transfer_to_opening(successor) {
        Ok(successor) => drop(successor),
        Err(recovery) => drop(recovery),
    }
}
