//! Host-independent native COM transport. A caller retains its own module,
//! server-operation, and publication permissions across these operations.

pub mod automation;
pub mod global_interface_table;
pub mod update_event;
