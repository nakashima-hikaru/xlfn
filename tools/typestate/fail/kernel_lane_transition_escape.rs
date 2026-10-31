// module: kernel/service_slot/transaction
use super::{LaneRetirement, LaneTransition, ReplaceableServiceSlot, NO_ACTIVE_LANE};

fn escape(slot: &ReplaceableServiceSlot<u32>) -> LaneRetirement<'_, u32> {
    let transition = LaneTransition::new(slot);
    transition.withdraw_to(NO_ACTIVE_LANE).unwrap() // error: E0515
}
