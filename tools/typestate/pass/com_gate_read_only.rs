// module: excel_rtd/windows
use super::server_gate::{ServerOperationBarrier, ServerPhase};

fn phase(barrier: &ServerOperationBarrier) -> ServerPhase {
    barrier.phase()
}
