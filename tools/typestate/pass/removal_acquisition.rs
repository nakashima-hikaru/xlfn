// module: runtime
use super::capabilities::ShutdownDeps;
use super::orchestration::OpenRollbackAcquisition;
use super::shutdown::{FinalRemoval, OpenRollback, RemovalOwner};

fn final_removal<A: crate::Addin>(deps: ShutdownDeps<'_, A>) -> Option<RemovalOwner<'_, A, FinalRemoval>> {
    RemovalOwner::acquire_final(deps)
}

fn rollback<A: crate::Addin>(deps: ShutdownDeps<'_, A>) -> OpenRollbackAcquisition<RemovalOwner<'_, A, OpenRollback>> {
    RemovalOwner::acquire_rollback(deps)
}
