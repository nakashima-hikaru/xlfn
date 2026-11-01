// module: runtime/shutdown/pipeline
use super::RemovalClaimGuard;
use crate::module_runtime::ModuleCleanupAuthority;

fn invalid<A: crate::Addin>(claim: RemovalClaimGuard<'_, A>, first: ModuleCleanupAuthority, second: ModuleCleanupAuthority) {
    let returned = claim.return_module_authority(first);
    let _ = returned.return_module_authority(second); // error: E0599
}
