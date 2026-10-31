// module: kernel/service_slot
use super::transaction::DrainedReplacement;
fn forge(source: DrainedReplacement<'_, u32>) {
    let _forged = DrainedReplacement { ..source }; // error: E0451
}
