// module: kernel/service_slot
use super::transaction::DrainedReplacement;
fn retarget(mut transaction: DrainedReplacement<'_, u32>, other: DrainedReplacement<'_, u32>) {
    transaction.transition = other.transition; // error: E0616
}
