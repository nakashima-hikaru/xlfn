// module: runtime/shutdown/pipeline
use super::RemovalClaimGuard;
use crate::module_runtime::ModuleCleanupAuthority;

fn return_once<A: crate::Addin>(claim: RemovalClaimGuard<'_, A>, authority: ModuleCleanupAuthority) {
    let returned = claim.return_module_authority(authority);
    drop(returned);
}
