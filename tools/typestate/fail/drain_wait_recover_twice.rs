// module: excel_rtd/windows
use crate::excel_rtd::drain_wait::UnstartedDrain;
fn invalid<F>(startup: UnstartedDrain<F>) {
    let _first = startup.into_work();
    let _second = startup.into_work(); // error: E0382
}
