// module: excel_rtd/windows/server_gate
fn invalid(barrier: &super::ServerOperationBarrier) {
    let _ = barrier.acquire_resource_teardown(); // error: E0599
}
