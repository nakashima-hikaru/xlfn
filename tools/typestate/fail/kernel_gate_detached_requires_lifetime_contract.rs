// module: kernel/drain_gate
use super::DrainGate;
fn invalid(gate: &DrainGate) {
    let _permit = gate.try_enter_detached(); // error: E0133
}
