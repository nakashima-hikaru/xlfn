// module: excel_rtd/windows
use crate::excel_rtd::drain_wait::{DrainWaitStartup, with_drain_wait};
fn recover<F>(startup: DrainWaitStartup<'_, F>) -> F { startup.recover() }
fn ordered() {
    let value = with_drain_wait(7_u32, recover);
    assert_eq!(value, 7);
}
