//! Unique publication reservation agrees with the atomic current view.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(publication {
    fields {
        #[sharding(variable)] pub mode: Option<bool>,
        #[sharding(option)] pub reservation: Option<bool>,
    }
    #[invariant] pub fn agrees(&self) -> bool { self.mode == self.reservation }
    init! { initialize() { init mode = None; init reservation = None; } }
    transition! { reserve(index: bool) {
        require(pre.mode.is_none()); update mode = Some(index); add reservation += Some(index);
    } }
    transition! { finish(index: bool) {
        remove reservation -= Some(index); update mode = None;
    } }
    property! { reserved(index: bool) {
        have reservation >= Some(index); assert(pre.mode == Some(index));
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(reserve)] fn reserve_inductive(pre: Self, post: Self, index: bool) {}
    #[inductive(finish)] fn finish_inductive(pre: Self, post: Self, index: bool) {}
});
}
