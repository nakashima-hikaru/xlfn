// module: kernel/service_slot/transaction
use super::{DrainedReplacement, RetiringReplacement};
fn forge(transaction: RetiringReplacement<'_, u32>) {
    let _drained = DrainedReplacement { transition: transaction.transition }; // error: E0451
}
