// module: runtime
use super::{LogicalUnload, PhysicalUnload, Runtime, UnloadPolicy};

struct PhysicalAddin;
impl crate::Addin for PhysicalAddin {
    type RtdSubscription = ();
    type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
    type SharedState = ();
    type LifecycleState = ();
    type Error = crate::XllError;
    type Layers = ();
    type AsyncExecutor = crate::NoAsyncExecutor;

    fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
        Ok(crate::Opened::new(()))
    }
}

#[allow(unsafe_code, reason = "The fixture has no application state or executable sources")]
// SAFETY: this add-in owns only () and starts no callbacks, threads, or timers.
unsafe impl crate::PhysicallyUnloadableAddin for PhysicalAddin {}

static LOGICAL: Runtime<PhysicalAddin, LogicalUnload> = Runtime::new();
static PHYSICAL: Runtime<PhysicalAddin, PhysicalUnload> =
    Runtime::<PhysicalAddin>::new_with_physical_unload();
static MACRO_PHYSICAL: crate::__private::v1::MacroRuntime<PhysicalAddin, PhysicalUnload> =
    crate::__private::v1::MacroRuntime::<PhysicalAddin>::new_with_physical_unload();

fn accept_either_policy<P: UnloadPolicy<PhysicalAddin>>(runtime: &Runtime<PhysicalAddin, P>) {
    let _ = runtime.phase();
}

fn use_both() {
    accept_either_policy(&LOGICAL);
    accept_either_policy(&PHYSICAL);
    accept_either_policy(MACRO_PHYSICAL.runtime());

    // Residency anchors must select the same policy as their runtime.
    let _: fn(&Runtime<PhysicalAddin, LogicalUnload>) -> i32 =
        crate::boundary::host::host_auto_close::<PhysicalAddin>;
    let _: fn(&Runtime<PhysicalAddin, PhysicalUnload>) -> i32 =
        crate::boundary::host::host_auto_remove::<PhysicalAddin>;
}
