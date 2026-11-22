// module: async_udf/boundary
#![allow(unsafe_code, reason = "compile-only rejection of a substituted async launch issuer")]
use crate::runtime::{AsyncCallGuard, CallGuard, Runtime};

fn prepare<A: crate::Addin>(
    _: &CallGuard<'_, A>,
    _: crate::generation::ExecutionLease<A>,
    _: crate::cancellation::CancellationToken,
) -> crate::XllResult<std::future::Ready<crate::XllResult<f64>>> {
    Ok(std::future::ready(Ok(1.0)))
}

fn invalid<A: crate::Addin>(guard: &AsyncCallGuard<'_, A>, other: &'static Runtime<A>) {
    // SAFETY: compile-only invalid caller, never executed.
    unsafe { super::run_admitted_async(guard, "test", "TEST", std::ptr::null_mut(), super::PlainStart(prepare::<A>), other); } // error: E0061
}
