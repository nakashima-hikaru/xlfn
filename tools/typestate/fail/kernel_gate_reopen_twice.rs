// module: kernel/drain_gate
use super::DrainGate;
fn invalid(gate: &DrainGate) {
    let drained = gate.control().seal().wait();
    drop(drained.reopen());
    drop(drained.reopen()); // error: E0382
}
