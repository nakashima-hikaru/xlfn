#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! FFI return allocation and free boundaries.

pub(crate) use super::{
    ffi_boundary, ffi_boundary_void, free_return, free_return_boundary, udf_boundary_named,
};
