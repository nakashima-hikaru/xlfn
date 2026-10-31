// module: kernel/service_slot
use super::transaction::LaneRetirement;

fn reclaim(transaction: LaneRetirement<'_, u32>) {
    let _retired = transaction.reclaim(); // error: E0599
}
