// module: excel_rtd/windows
use super::server_gate::TerminationWorkerStart;

fn invalid(start: TerminationWorkerStart<'static>) {
    std::thread::spawn(move || drop(start)); // error: E0277
}
