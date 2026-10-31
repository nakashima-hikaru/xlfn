// module: kernel/drain_gate
use crate::operation_gate::OperationGate;
fn invalid(gate: &OperationGate) {
    let _ = gate.acquire(); // error: E0599
}
