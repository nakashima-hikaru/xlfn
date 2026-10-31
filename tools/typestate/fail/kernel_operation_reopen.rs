// module: kernel/drain_gate
use crate::operation_gate::OperationGate;
fn invalid(gate: &OperationGate) {
    gate.reopen(); // error: E0599
}
