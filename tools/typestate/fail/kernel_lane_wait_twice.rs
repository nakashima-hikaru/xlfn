// module: kernel/service_slot
use super::transaction::LaneRetirement;

fn drain_twice(transaction: LaneRetirement<'_, u32>) {
    let _first = transaction.wait();
    let _second = transaction.wait(); // error: E0382
}
