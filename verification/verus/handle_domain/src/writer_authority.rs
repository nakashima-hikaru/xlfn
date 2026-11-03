//! The writer capability and atomic view agree; changing either requires both.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(publication<T> {
    fields {
        #[sharding(variable)] pub atomic: *mut T,
        #[sharding(variable)] pub writer: *mut T,
    }
    #[invariant] pub fn views_agree(&self) -> bool { self.atomic == self.writer }
    init! { initialize(pointer: *mut T) { init atomic = pointer; init writer = pointer; } }
    transition! { set(pointer: *mut T) { update atomic = pointer; update writer = pointer; } }
    property! { agree() { assert(pre.atomic == pre.writer); } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, pointer: *mut T) {}
    #[inductive(set)] fn set_inductive(pre: Self, post: Self, pointer: *mut T) {}
});
}
