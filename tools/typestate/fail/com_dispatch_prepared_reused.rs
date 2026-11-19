// module: excel_rtd/windows
use crate::drain_wait::windows::DispatchPrepared;

fn invalid(prepared: DispatchPrepared<fn() -> u32>) {
    let _ = prepared.run();
    let _ = prepared.run(); // error: E0382
}
