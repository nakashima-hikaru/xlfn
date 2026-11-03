// module: excel_rtd/windows
use super::server_gate::ServerOperationBarrier;

fn invalid(barrier: &ServerOperationBarrier, owner: std::thread::ThreadId) {
    let _ = barrier.wait_for_deferred_termination(owner, 1); // error: E0624
}
