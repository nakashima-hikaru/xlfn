// module: runtime
#![allow(unsafe_code, reason = "negative fixture checks the private raw constructor")]
use super::{Runtime, CallGuard};
use crate::generation::{ExecutionGeneration, ExecutionLease};
use crate::lifecycle::LifecycleCoordinator;
use std::ptr::NonNull;

fn invalid<A: crate::Addin>(
    runtime: &'static Runtime<A>,
    call: &CallGuard<'_, A>,
    issuer: &'static LifecycleCoordinator<A>,
    generation: NonNull<ExecutionGeneration<A>>,
    permit: xlfn_kernel::drain_gate::StripedOwnedDrainPermit<
        { xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT },
    >,
) {
    let _ = runtime.execution_lease(call); // error: E0599
    let _ = issuer.acquire_execution_lease(generation); // error: E0599
    let _ = unsafe { ExecutionLease::new(generation, permit) }; // error: E0624
}
