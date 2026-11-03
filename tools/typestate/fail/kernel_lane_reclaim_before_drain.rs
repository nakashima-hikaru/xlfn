// module: kernel/service_slot
use super::transaction::RetiringReplacement;

fn reclaim(transaction: RetiringReplacement<'_, u32>) {
    let _retired = transaction.reclaim(); // error: E0599
}
