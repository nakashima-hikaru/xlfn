// module: kernel/drain_gate
use super::DrainGate;
fn invalid(first: &DrainGate, second: &DrainGate) {
    let mut control = first.control();
    control.gate = second; // error: E0616
}
