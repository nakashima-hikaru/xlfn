//! Instantiate shared call retention with real nonduplicable gate permits.
use vstd::prelude::*;
use super::retained_permits::Retained;
use super::rotation::gate_permits::admission;
verus! {
pub(crate) open spec fn contains(held: &Retained<Tracked<admission::permits>>, id: vstd::tokens::InstanceId) -> bool {
    exists|index: int| 0 <= index < held.view().len() && (#[trigger] held.view()[index])@.instance_id() == id
}
pub(crate) fn retain(held: &mut Retained<Tracked<admission::permits>>, Tracked(permit): Tracked<admission::permits>)
    ensures final(held).view() == old(held).view().push(Tracked(permit)),
        contains(final(held), permit.instance_id()),
        forall|id: vstd::tokens::InstanceId| #[trigger] contains(old(held), id) ==> contains(final(held), id),
{
    held.insert(Tracked(permit));
    assert(contains(held, permit.instance_id())) by {
        assert(held.view()[old(held).view().len() as int]@.instance_id() == permit.instance_id());
    }
    assert forall|id: vstd::tokens::InstanceId| #[trigger] contains(old(held), id) implies contains(held, id) by {
        let index = choose|index: int| 0 <= index < old(held).view().len()
            && (#[trigger] old(held).view()[index])@.instance_id() == id;
        assert(held.view()[index] == old(held).view()[index]);
    }
}
}
