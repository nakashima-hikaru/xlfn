//! Token generation for the derive and attribute façades.

mod enum_;
mod handle;

pub(super) use enum_::expand_excel_enum;
pub(super) use handle::expand_excel_handle_object;
