// module: kernel/lib
use crate::thread_affine::{
    BindingOccupied, BindingState, BindingTxn, ThreadAffineAccess, ThreadAffineSlot,
};
use std::cell::Cell;
use std::rc::Rc;

fn payload_subtyping_still_works(value: for<'a> fn(&'a ())) -> fn(&'static ()) {
    value
}

fn cross_thread_root_for_local_payload() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ThreadAffineSlot<Rc<Cell<u32>>>>();
}

fn shorten_slot<'long: 'short, 'short, T: 'static>(
    slot: &'long ThreadAffineSlot<T>,
) -> &'short ThreadAffineSlot<T> {
    slot
}

fn shorten_access<'long: 'short, 'short, T: 'static>(
    access: ThreadAffineAccess<'long, T>,
) -> ThreadAffineAccess<'short, T> {
    access
}

fn shorten_transaction<'long: 'short, 'short, T: 'static>(
    transaction: BindingTxn<'long, T, BindingOccupied>,
) -> BindingTxn<'short, T, BindingOccupied> {
    transaction
}

fn shorten_state<'long: 'short, 'short, T: 'static>(
    state: BindingState<'long, T>,
) -> BindingState<'short, T> {
    state
}
