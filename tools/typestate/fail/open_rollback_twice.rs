// module: runtime
use super::open_txn::OpenRollbackOwner;
use super::AddinLifecycleAccess;

fn invalid<A: crate::Addin>(rollback: OpenRollbackOwner<'_, A>, lifecycle: &AddinLifecycleAccess<'_, A>) {
    rollback.run(lifecycle);
    rollback.run(lifecycle); // error: E0382
}
