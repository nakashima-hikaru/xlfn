// module: excel_rtd/windows
use super::server_gate::DeferredTerminationStart;

fn invalid(start: DeferredTerminationStart<'_, '_>) {
    start.commit(); // error: E0599
}
