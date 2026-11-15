// module: excel_rtd/windows
use super::event::DispatchPrepared;

fn invalid(prepared: DispatchPrepared<fn() -> u32>) {
    let _ = prepared.run();
    let _ = prepared.run(); // error: E0382
}
