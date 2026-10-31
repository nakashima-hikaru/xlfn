// module: runtime
use xlfn_kernel::drain_gate::DrainGate;
fn invalid(gate: &DrainGate) {
    let _ = gate.reopen(); // error: E0624
}
