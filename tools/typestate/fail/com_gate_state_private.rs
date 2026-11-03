// module: excel_rtd/windows
use super::server_gate::{ServerOperationBarrier, ServerPhase};

fn invalid(barrier: &ServerOperationBarrier) {
    barrier.state.lock().phase = ServerPhase::Terminated; // error: E0616
    barrier.state.lock().in_flight = 0; // error: E0616
}
