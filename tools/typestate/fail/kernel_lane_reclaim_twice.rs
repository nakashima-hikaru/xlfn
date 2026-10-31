// module: kernel/service_slot
use super::transaction::DrainedLane;

fn reclaim_twice(transaction: DrainedLane<'_, u32>) {
    let _first = transaction.reclaim();
    let _second = transaction.reclaim(); // error: E0382
}
