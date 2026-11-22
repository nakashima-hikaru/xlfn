#[doc(inline)]
pub use xlfn_engine::diagnostic_id::DiagnosticId;
#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use xlfn_engine::diagnostic_id::internal::with_low_u32;
pub(crate) use xlfn_engine::diagnostic_id::internal::{from_ascii8, from_u64};
