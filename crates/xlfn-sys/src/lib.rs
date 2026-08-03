//! Raw, layout-stable definitions for the Excel 12 XLL ABI.
//!
//! This crate is the only framework crate that talks directly to Excel's
//! `MdCallBack12` entry point. Higher layers should use `xlfn-core`.
//!
//! The layout follows Microsoft's XLL SDK declarations in
//! [`XLCALL.H`](https://learn.microsoft.com/en-us/office/client-developer/excel/xlcall-h).
//! Enable `abi-probe` only for the repository's native ABI cross-check; normal
//! add-ins resolve the callback from the Excel host at runtime.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_os = "windows")]
mod win32;

mod callback;
mod types;

pub use callback::{
    XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS, XLRET_UNCALCED, excel_free, excel12,
    excel12_async_return, excel12_with_invocation,
};

#[cfg(feature = "abi-probe")]
pub use callback::install_callback_for_abi_probe;

pub use types::*;
