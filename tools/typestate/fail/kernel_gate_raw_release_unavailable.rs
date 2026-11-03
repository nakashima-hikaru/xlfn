// module: runtime
use xlfn_kernel::drain_gate::StripedDrainGate;
fn invalid(gate: &StripedDrainGate<2>) {
    gate.release(0); // error: E0624
}
