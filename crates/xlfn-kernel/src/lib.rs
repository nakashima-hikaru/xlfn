//! Implementation-only ownership and concurrency primitives for `xlfn`.
//!
//! This crate deliberately does not depend on `xlfn`, `xlfn-sys`, or any Excel
//! and COM integration layer. Its public items are consumed by the `xlfn`
//! adapter and are not a supported direct application API.

#![deny(unsafe_code)]

#[allow(
    unsafe_code,
    reason = "striped drain counter acquires and seals per-stripe reader scopes"
)]
pub mod drain_gate;
pub mod invariant;
pub mod operation_gate;
pub mod published_owner;
pub mod quota;
#[allow(
    unsafe_code,
    reason = "rollback drop guard destroys uninitialized/moved host state"
)]
pub mod rollback;
pub mod rotating_read_domain;
pub mod sealable_counter;
#[allow(
    unsafe_code,
    reason = "service slot synchronizes aligned storage initialization and destruction"
)]
pub mod service_slot;
pub mod sync;
pub mod thread_affine;
