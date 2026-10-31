// module: kernel/service_slot/transaction
use super::{LaneTransition, ReplaceableServiceSlot, NO_ACTIVE_LANE};

fn drop_before_reclaim(slot: &ReplaceableServiceSlot<u32>) {
    let transition = LaneTransition::new(slot);
    let drained = transition.withdraw_to(NO_ACTIVE_LANE).unwrap().wait();
    drop(transition); // error: E0505
    let _retired = drained.reclaim();
}
