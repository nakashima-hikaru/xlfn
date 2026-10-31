// module: kernel/service_slot/transaction
use super::{ReplacementTxn, RetiringReplacement, ReplaceableServiceSlot};
fn escape<'a>() -> RetiringReplacement<'a, u32> {
    let slot = ReplaceableServiceSlot::new();
    ReplacementTxn::new(&slot, Box::new(8)).prepare(|_, _| Ok::<_, ()>(())).unwrap().publish() // error: E0515
}
