// module: kernel/service_slot/transaction
use super::{LaneTransition, ReplaceableServiceSlot, NO_ACTIVE_LANE};

fn retire(slot: &ReplaceableServiceSlot<u32>) {
    let transition = LaneTransition::new(slot);
    if let Some(retirement) = transition.withdraw_to(NO_ACTIVE_LANE) {
        let _retired = retirement.wait().reclaim();
    }
}
