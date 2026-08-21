#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! Excel callback registration and unregistration operations.

pub(crate) use super::{EventRegistration, HostRegistrar, UnregisterResult};
