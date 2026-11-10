// module: runtime
use super::open_txn::{Begun, OpeningTxn};

fn open<'runtime, A: crate::Addin>(
    transaction: OpeningTxn<'runtime, A, Begun<A>>,
    host: crate::host_callback::HostCallbackSession,
    lifecycle_state: A::LifecycleState,
    inputs: crate::runtime_components::GenerationServiceInputs<
        crate::addin_impl::AddinSubscription<A>,
        crate::addin_impl::AddinSources<A>,
    >,
    generation: crate::generation::OpeningGeneration<A>,
    access: &super::AddinLifecycleAccess<'_, A>,
    registrations: crate::registration::SuccessfulRegistrations,
) -> Option<()> {
    transaction
        .attach_host(host)
        .begin_initialization()
        .initialized(lifecycle_state, inputs)
        .stage_opening_generation(generation)
        .ok()?
        .install_lifecycle(access)
        .ok()?
        .stage_host_mutations(registrations)
        .commit()
        .ok()
}
