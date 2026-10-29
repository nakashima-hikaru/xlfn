// module: runtime
use super::capabilities::OpenDeps;
use super::open_txn::OpeningTxn;
use super::AddinLifecycleAccess;
use crate::generation::RemovalEpoch;

fn ordered<A: crate::Addin>(deps: OpenDeps<'_, A>, epoch: RemovalEpoch, lifecycle: &AddinLifecycleAccess<'_, A>) {
    let Ok(opening) = OpeningTxn::acquire(deps, epoch) else { return };
    opening.attach_host(crate::host_callback::HostCallbackSession::new()).into_rollback().run(lifecycle);
}
