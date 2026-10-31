// module: kernel/service_slot
use super::transaction::ReplacementTxn;
fn publish(transaction: ReplacementTxn<'_, u32>) {
    let _retiring = transaction.publish(); // error: E0599
}
