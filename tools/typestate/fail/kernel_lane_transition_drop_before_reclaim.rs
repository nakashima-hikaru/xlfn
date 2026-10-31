// module: kernel/service_slot/transaction
use super::{ReplacementTxn, ReplaceableServiceSlot};
fn reuse(slot: &ReplaceableServiceSlot<u32>) {
    let prepared = ReplacementTxn::new(slot, Box::new(8)).prepare(|_, _| Ok::<_, ()>(())).unwrap();
    let retirement = prepared.publish();
    drop(prepared); // error: E0382
    let _retired = retirement.wait().reclaim();
}
