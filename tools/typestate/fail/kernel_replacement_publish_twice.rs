// module: kernel/service_slot
use super::transaction::PreparedReplacement;
fn publish(transaction: PreparedReplacement<'_, u32>) {
    let _first = transaction.publish();
    let _second = transaction.publish(); // error: E0382
}
