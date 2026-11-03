// module: kernel/drain_gate
use super::StripedDrainGate;
fn invalid(gate: &StripedDrainGate<2>) {
    gate.control().seal().reopen(); // error: E0599
}
