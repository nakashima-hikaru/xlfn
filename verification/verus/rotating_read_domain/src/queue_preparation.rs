//! Linear open/prepared authority for one reusable queue; no epoch can reuse a live ticket.
use vstd::prelude::*;
use verus_state_machines_macros::tokenized_state_machine;
verus! {
tokenized_state_machine!(phase {
    fields {
        #[sharding(variable)] pub state: Option<Set<vstd::tokens::InstanceId>>,
        #[sharding(option)] pub ready: Option<()>,
        #[sharding(option)] pub prepared: Option<Set<vstd::tokens::InstanceId>>,
    }
    #[invariant] pub fn exact(&self) -> bool {
        self.prepared == self.state && self.ready.is_some() == self.state.is_none()
    }
    init! { initialize() { init state = None; init ready = Some(()); init prepared = None; } }
    transition! { freeze(bound: Set<vstd::tokens::InstanceId>) {
        remove ready -= Some(()); update state = Some(bound); add prepared += Some(bound);
    } }
    transition! { reset(bound: Set<vstd::tokens::InstanceId>) {
        remove prepared -= Some(bound); update state = None; add ready += Some(());
    } }
    property! { is_ready() { have ready >= Some(()); assert(pre.state.is_none()); } }
    property! { is_prepared(bound: Set<vstd::tokens::InstanceId>) {
        have prepared >= Some(bound); assert(pre.state == Some(bound));
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self) {}
    #[inductive(freeze)] fn freeze_inductive(pre: Self, post: Self, bound: Set<vstd::tokens::InstanceId>) {}
    #[inductive(reset)] fn reset_inductive(pre: Self, post: Self, bound: Set<vstd::tokens::InstanceId>) {}
});
}
