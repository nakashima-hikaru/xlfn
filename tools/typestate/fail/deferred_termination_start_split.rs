// module: excel_rtd/windows
use super::server_gate::DeferredTerminationStart;

fn invalid(start: DeferredTerminationStart<'_, '_>) {
    let _ = start.prepared; // error: E0616
    let _ = start.work; // error: E0616
}
