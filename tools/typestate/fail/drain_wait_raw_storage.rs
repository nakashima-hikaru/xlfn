// module: excel_rtd/windows
use crate::excel_rtd::drain_wait::DrainWaitStartup;
fn invalid<F>(startup: DrainWaitStartup<'_, F>) {
    let _storage = startup.slot; // error: E0616
}
