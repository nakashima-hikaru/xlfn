// module: excel_rtd/windows
use super::server::RtdServer;

fn teardown(server: &RtdServer) {
    let Ok(access) = server.drain_access() else { return };
    let _ = access.teardown_resources();
    let Ok(joined) = access.join_worker() else { return };
    let _ = joined.worker_panicked();
    let _ = joined.teardown_and_remove_active(false);
}
