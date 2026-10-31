// module: kernel/service_slot
use super::transaction::{InitializingTxn, SealingTxn, LaneRetirement};
use super::{GenerationServiceSlot, ReplaceableServiceSlot};

fn initialize(transaction: InitializingTxn<'_, (), u32, ()>) {
    let _read = transaction.commit(Box::new(7), |_| {});
}

fn reject(transaction: InitializingTxn<'_, (), u32, ()>) {
    let _fault = transaction.fail(());
}

fn seal(transaction: SealingTxn<'_, (), u32, ()>) {
    let _sealed = transaction.wait().finish(|_| Ok(()));
}

fn retire(transaction: LaneRetirement<'_, u32>) {
    let _retired = transaction.wait().reclaim();
}

fn public_slots() {
    let slot = GenerationServiceSlot::<(), u32, ()>::new();
    slot.arm(()).unwrap();
    drop(slot.read(|()| Ok(Box::new(7)), |_| {}).unwrap());
    let _sealed = slot.seal(|| (), |_| Ok(())).unwrap();
    let replaceable = ReplaceableServiceSlot::new();
    let _previous = replaceable.replace_with(Box::new(8_u32), |_, _| Ok::<(), ()>(())).unwrap();
    let _retired = replaceable.close();
}
