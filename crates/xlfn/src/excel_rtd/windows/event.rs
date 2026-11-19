//! RTD barrier events share the same COM wait implementation as async shutdown.

pub(super) use crate::drain_wait::windows::{ManualResetEvent, Win32EventError};
