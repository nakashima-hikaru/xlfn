// module: kernel/service_slot
use super::transaction::RetiringReplacement;

fn drain_twice(transaction: RetiringReplacement<'_, u32>) {
    let _first = transaction.wait();
    let _second = transaction.wait(); // error: E0382
}
