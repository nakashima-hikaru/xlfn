// module: runtime
use crate::lifecycle::{LifecycleControl, OpenStart};
use crate::generation::RemovalEpoch;
use crate::runtime::{Begun, OpeningTxn};

fn invalid<'a, A: crate::Addin>(control: &LifecycleControl<'a, A>, successor: OpeningTxn<'a, A, Begun<A>>) {
    let start: OpenStart<'a, A> = control.begin_open(RemovalEpoch::new(0)).unwrap();
    if let Err(failure) = start.transfer_to_opening(successor) {
        drop(failure._start); // error: E0616
    }
}
