// module: kernel/drain_gate
use super::DrainGate;
fn invalid(gate: &DrainGate) {
    gate.control().reopen(); // error: E0599
}
