// module: kernel/drain_gate
use super::StripedDrainGate;
fn invalid(gate: &StripedDrainGate<2>) {
    let permit = gate.try_enter(1).unwrap();
    permit.release();
    permit.release(); // error: E0382
}
