// module: async_udf
#![allow(unsafe_code, reason = "compile-only valid async launch ordering")]
use crate::runtime::AsyncCallGuard;

fn launch<A: crate::Addin>(guard: &AsyncCallGuard<'_, A>) {
    // SAFETY: compile-only fixture, never executed.
    unsafe { super::boundary::async_udf_boundary_named_inner(guard, "test", "TEST", std::ptr::null_mut(), |_, _, _| Ok(async { Ok::<f64, crate::XllError>(1.0) })); }
}
