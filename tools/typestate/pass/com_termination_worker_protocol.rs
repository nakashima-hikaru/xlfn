// module: excel_rtd/windows
use super::server_gate::{ServerCloseError, TerminationWorker};

fn start_and_join(
    worker: &TerminationWorker,
    handle: std::thread::JoinHandle<()>,
) -> Result<(), ServerCloseError> {
    worker.reserve_start()?.commit(handle);
    worker.join()
}
