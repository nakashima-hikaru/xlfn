// module: excel_rtd/windows
use super::server_gate::DeferredTerminationStart;

fn invalid(start: DeferredTerminationStart<'_, '_>) {
    let _ = start.spawn();
    let _ = start.spawn(); // error: E0382
}
