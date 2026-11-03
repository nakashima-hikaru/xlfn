// module: async_udf
#![allow(unsafe_code, reason = "compile-only rejection of a substituted async launch issuer")]
use crate::runtime::{AsyncCallGuard, Runtime};

fn invalid<A: crate::Addin>(guard: &AsyncCallGuard<'_, A>, other: &'static Runtime<A>) {
    // SAFETY: compile-only invalid caller, never executed.
    unsafe { super::boundary::async_udf_boundary_named_inner(guard, "test", "TEST", std::ptr::null_mut(), |_, _, _| Ok(async { Ok::<f64, crate::XllError>(1.0) }), other); } // error: E0061
}
