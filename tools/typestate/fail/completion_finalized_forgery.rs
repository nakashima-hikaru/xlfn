// module: runtime/shutdown/pipeline
use super::super::finalization::Finalized;
use super::super::certificate::{ClosedWitness, FinalRemoval};
use super::RemovalClaimGuard;
use crate::runtime::capabilities::ShutdownDeps;
fn invalid<'a, A: crate::Addin>(deps: ShutdownDeps<'a,A>, witness: ClosedWitness, claim: RemovalClaimGuard<'a,A>) {
    let _: Finalized<'a,A,FinalRemoval> = Finalized { deps, completed: (witness, claim) }; // error: E0451
}
