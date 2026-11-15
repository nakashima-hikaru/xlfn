// module: excel_rtd/windows
use crate::excel_rtd::drain_wait::UnstartedDrain;
fn invalid<F>(startup: UnstartedDrain<F>) {
    let _storage = startup.0; // error: E0616
}
