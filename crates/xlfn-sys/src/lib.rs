//! Raw, layout-stable definitions for the Excel 12 XLL ABI.
//!
//! This crate is the only framework crate that talks directly to Excel's
//! `MdCallBack12` entry point. Higher layers should use `xlfn-core`.

#![allow(non_camel_case_types, non_snake_case)]
#![deny(unsafe_op_in_unsafe_fn)]

mod callback;
mod types;

pub use callback::{
    XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS, XLRET_UNCALCED, excel_free, excel12,
    excel12_with_invocation,
};

#[cfg(feature = "abi-probe")]
pub use callback::install_callback_for_abi_probe;

pub use types::*;
