// module: excel_rtd/windows
use super::server_gate::TerminationWorker;

fn invalid(worker: &TerminationWorker) {
    let _ = &worker.state; // error: E0616
}
