// module: kernel/service_slot/transaction
use super::{ReplacementTxn, ReplaceableServiceSlot};
fn replace(slot: &ReplaceableServiceSlot<u32>) {
    let prepared = ReplacementTxn::new(slot, Box::new(8)).prepare(|_, _| Ok::<_, ()>(())).unwrap();
    let _retired = prepared.publish().wait().reclaim();
}
