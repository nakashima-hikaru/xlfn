// module: kernel/service_slot
use super::transaction::DrainedLane;
use crate::published_owner::PublishedOwner;

fn substitute_owner(transaction: DrainedLane<'_, u32>, other: PublishedOwner<u32>) {
    let _retired = transaction.reclaim(other); // error: E0061
}
