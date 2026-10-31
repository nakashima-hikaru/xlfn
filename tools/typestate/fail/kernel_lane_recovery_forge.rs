// module: kernel/service_slot
use super::transaction::DrainedLane;
use super::ReplaceableLane;
use crate::sync::MutexGuard;

fn forge_drained<'a>(lane: &'a ReplaceableLane<u32>, guard: &'a MutexGuard<'a, ()>) {
    let _forged = DrainedLane { lane, _transition: guard }; // error: E0451
}
