// module: runtime
use crate::lifecycle::OpenStart;
use crate::runtime::{Begun, OpeningTxn};

fn invalid<'a, A: crate::Addin>(start: OpenStart<'a, A>, successor: OpeningTxn<'a, A, Begun<A>>) {
    let _handoff = start.transfer_to_opening(successor);
    drop(start); // error: E0382
}
