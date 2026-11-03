// module: kernel/drain_gate
use super::DrainGate;
fn invalid(gate: &DrainGate) {
    let control = gate.control();
    let sealed = control.seal();
    let _again = control.seal(); // error: E0382
    drop(sealed);
}
