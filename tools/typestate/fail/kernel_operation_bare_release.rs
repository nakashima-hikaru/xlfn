// module: kernel/drain_gate
use crate::operation_gate::OperationGate;
fn invalid(gate: &OperationGate) {
    gate.release(); // error: E0599
}
