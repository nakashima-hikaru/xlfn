// module: runtime
use super::open_txn::OpenRollbackOwner;
use super::{AddinLifecycleAccess, Runtime, UnloadPolicy};

fn invalid<A: crate::Addin>(rollback: OpenRollbackOwner<'_, A>, other: &Runtime<A, impl UnloadPolicy<A>>, lifecycle: &AddinLifecycleAccess<'_, A>) {
    rollback.run(other, lifecycle); // error: E0061
}
