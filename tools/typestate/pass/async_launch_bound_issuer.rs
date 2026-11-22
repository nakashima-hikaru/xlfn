// module: async_udf/boundary
#![allow(unsafe_code, reason = "compile-only valid async launch ordering")]
use crate::runtime::{AsyncCallGuard, CallGuard};

fn prepare<A: crate::Addin>(
    _: &CallGuard<'_, A>,
    _: crate::generation::ExecutionLease<A>,
    _: crate::cancellation::CancellationToken,
) -> crate::XllResult<std::future::Ready<crate::XllResult<f64>>> {
    Ok(std::future::ready(Ok(1.0)))
}

fn launch<A: crate::Addin>(guard: &AsyncCallGuard<'_, A>) {
    // SAFETY: compile-only fixture, never executed.
    unsafe { super::run_admitted_async(guard, "test", "TEST", std::ptr::null_mut(), super::PlainStart(prepare::<A>)); }
}
