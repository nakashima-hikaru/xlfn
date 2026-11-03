// module: kernel/drain_gate
use super::{DrainGate, StripedDrainGate};

fn valid(single: &DrainGate, striped: &StripedDrainGate<2>) {
    let control = single.control();
    let sealed = control.seal();
    let drained = sealed.wait();
    drop(drained.reopen());
    let control = striped.control();
    let sealed = control.seal();
    let drained = sealed.wait();
    drop(drained.reopen());
    single.try_enter().unwrap().release();
    striped.try_enter(1).unwrap().release();
    let terminal = crate::operation_gate::OperationGate::new();
    let drained = terminal.close_and_wait_begin().wait();
    let repeated = drained;
    assert!(repeated.belongs_to(&terminal));
}
