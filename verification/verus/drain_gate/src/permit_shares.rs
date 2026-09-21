//! Owned observation shares retain the exact linear gate admission in storage.
//! This lets an atomic invariant retain a witness without extending a Rust borrow.
use vstd::prelude::*;
use vstd::multiset::*;
use verus_state_machines_macros::tokenized_state_machine;
use super::permits::admission;
verus! {
tokenized_state_machine!(retained_admission {
    fields {
        #[sharding(constant)] pub payload: admission::permits,
        #[sharding(storage_option)] pub held: Option<admission::permits>,
        #[sharding(option)] pub scope: Option<()>,
        #[sharding(multiset)] pub shares: Multiset<()>,
        #[sharding(variable)] pub outstanding: nat,
    }
    #[invariant] pub fn conservation(&self) -> bool {
        self.outstanding == self.shares.len()
        && self.held == if self.scope.is_some() { Some(self.payload) } else { None }
        && (self.outstanding > 0 ==> self.scope.is_some())
    }
    init! { initialize(permit: admission::permits) {
        init payload = permit; init held = Some(permit); init scope = Some(());
        init shares = Multiset::empty(); init outstanding = 0;
    } }
    transition! { issue() {
        have scope >= Some(());
        add shares += {()}; update outstanding = pre.outstanding + 1;
    } }
    transition! { finish() {
        remove shares -= {()}; assert(pre.outstanding > 0);
        update outstanding = (pre.outstanding - 1) as nat;
    } }
    transition! { close() {
        require(pre.outstanding == 0); remove scope -= Some(());
        withdraw held -= Some(pre.payload);
    } }
    property! { guard_share() {
        have shares >= {()}; guard held >= Some(pre.payload);
    } }
    #[inductive(initialize)] fn initialize_inductive(post: Self, permit: admission::permits) {}
    #[inductive(issue)] fn issue_inductive(pre: Self, post: Self) {}
    #[inductive(finish)] fn finish_inductive(pre: Self, post: Self) {}
    #[inductive(close)] fn close_inductive(pre: Self, post: Self) {}
});
pub tracked struct Scope {
    instance: retained_admission::Instance,
    owner: retained_admission::scope,
    count: retained_admission::outstanding,
}
pub tracked struct Share {
    instance: retained_admission::Instance,
    token: retained_admission::shares,
}
impl Scope {
    pub closed spec fn inv(&self) -> bool {
        self.owner.instance_id() == self.instance.id() && self.count.instance_id() == self.instance.id()
    }
    pub closed spec fn id(&self) -> vstd::tokens::InstanceId { self.instance.id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.instance.payload().instance_id() }
    pub closed spec fn len(&self) -> nat { self.count.value() }
    pub proof fn new(tracked permit: admission::permits) -> (tracked scope: Self)
        ensures scope.inv(), scope.gate_id() == permit.instance_id(), scope.len() == 0,
    {
        let tracked (Tracked(instance), Tracked(owner), Tracked(shares), Tracked(count))
            = retained_admission::Instance::initialize(permit, Some(permit));
        Scope { instance, owner: owner.tracked_unwrap(), count }
    }
    pub proof fn issue(tracked &mut self) -> (tracked share: Share)
        requires old(self).inv(),
        ensures final(self).inv(), final(self).id() == old(self).id(),
            final(self).gate_id() == old(self).gate_id(), final(self).len() == old(self).len() + 1,
            share.inv(), share.scope_id() == final(self).id(), share.gate_id() == final(self).gate_id(),
    {
        let tracked token = self.instance.issue(&self.owner, &mut self.count);
        Share { instance: self.instance.clone(), token }
    }
    pub proof fn finish(tracked &mut self, tracked share: Share)
        requires old(self).inv(), share.inv(), share.scope_id() == old(self).id(),
        ensures final(self).inv(), final(self).id() == old(self).id(),
            final(self).gate_id() == old(self).gate_id(), final(self).len() + 1 == old(self).len(),
    { self.instance.finish(share.token, &mut self.count); }
    pub proof fn close(tracked self) -> (tracked permit: admission::permits)
        requires self.inv(), self.len() == 0,
        ensures permit.instance_id() == self.gate_id(),
    { self.instance.close(self.owner, &self.count) }
}
impl Share {
    pub closed spec fn inv(&self) -> bool { self.token.instance_id() == self.instance.id() }
    pub closed spec fn scope_id(&self) -> vstd::tokens::InstanceId { self.instance.id() }
    pub closed spec fn gate_id(&self) -> vstd::tokens::InstanceId { self.instance.payload().instance_id() }
    pub proof fn permit(tracked &self) -> (tracked permit: &admission::permits)
        requires self.inv(), ensures permit.instance_id() == self.gate_id(),
    { self.instance.guard_share(&self.token) }
    pub proof fn positive(tracked &self, tracked gate: &admission::Instance, tracked active: &admission::active)
        requires self.inv(), self.gate_id() == gate.id(), active.instance_id() == gate.id(),
        ensures active.value() > 0,
    { gate.positive(active, self.permit()); }
}
/// Multiple binding observations share one actual admission. Completing one
/// observation does not release the gate or invalidate the other share.
pub proof fn two_observations_then_release(tracked gate: &admission::Instance,
    tracked active: &mut admission::active)
    requires old(active).instance_id() == gate.id(), old(active).value() == 0,
    ensures final(active).instance_id() == gate.id(), final(active).value() == 0,
{
    let tracked permit = gate.acquire(active);
    let tracked mut scope = Scope::new(permit);
    let tracked first = scope.issue();
    let tracked second = scope.issue();
    scope.finish(first);
    second.positive(gate, active);
    scope.finish(second);
    let tracked permit = scope.close();
    gate.release(active, permit);
}
}
