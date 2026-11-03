// module: excel_rtd/windows
use super::server::RtdServer;
use super::server_gate::ServerOperationBarrier;

fn invalid(server: &RtdServer, barrier: &ServerOperationBarrier) {
    let _ = server.teardown_resources(); // error: E0599
    let _ = barrier.teardown_resources(); // error: E0599
}
