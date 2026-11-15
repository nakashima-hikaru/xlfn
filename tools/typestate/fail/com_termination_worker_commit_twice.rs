// module: excel_rtd/windows
use super::server_gate::TerminationWorkerStart;

fn invalid(
    start: TerminationWorkerStart<'_>,
    first: std::thread::JoinHandle<()>,
    second: std::thread::JoinHandle<()>,
) {
    start.commit(first);
    start.commit(second); // error: E0382
}
