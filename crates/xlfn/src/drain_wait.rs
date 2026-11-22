//! Excel's native drain facade; protocol ownership lives in xlfn-host.
pub(crate) use xlfn_host::drain::{DrainWork, UnstartedDrain};

#[cfg(all(test, windows))]
pub(crate) use xlfn_host::drain::fail_next_drain_spawn;
#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
pub(crate) use windows::drain_with_com_dispatch;
