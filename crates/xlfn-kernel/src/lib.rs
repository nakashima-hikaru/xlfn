//! Implementation-only ownership and concurrency primitives for `xlfn`.
//!
//! This crate deliberately does not depend on `xlfn`, `xlfn-sys`, or any Excel
//! and COM integration layer. Its public items are consumed by the `xlfn`
//! adapter and are not a supported direct application API.

pub mod invariant;
pub mod operation_gate;
pub mod quota;
pub mod service_slot;
pub mod thread_affine;
