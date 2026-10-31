// module: kernel/service_slot
use super::transaction::DrainedLane;
use super::ReplaceableLane;

fn substitute_lane<'a>(mut transaction: DrainedLane<'a, u32>, other: &'a ReplaceableLane<u32>) {
    transaction.lane = other; // error: E0616
}
