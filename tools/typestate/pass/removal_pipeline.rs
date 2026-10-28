// module: runtime
use super::shutdown::{FinalRemoval, RemovalOwner};
use super::{AddinLifecycleAccess, Runtime, UnloadPolicy};

fn ordered<A: crate::Addin>(
    owner: RemovalOwner<'_, A>,
    runtime: &Runtime<A, impl UnloadPolicy<A>>,
    lifecycle: &AddinLifecycleAccess<'_, A>,
) {
    let Ok(transaction) = owner.drain::<FinalRemoval>() else { return };
    let Ok(transaction) = transaction.stop_producers(|_| {}) else { return };
    let transaction = transaction.close_module_callbacks();
    let Ok(transaction) = transaction.detach_host() else { return };
    let Ok(transaction) = transaction.quiesce_addin(runtime, lifecycle) else { return };
    let Ok(transaction) = transaction.seal_services() else { return };
    let mut report = crate::shutdown::CloseReport::default();
    let Ok(transaction) = transaction.cleanup_addin(lifecycle, &mut report) else { return };
    let Ok(transaction) = transaction.finish_services() else { return };
    drop(transaction);
}
