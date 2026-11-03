// module: runtime
use crate::module_runtime::{ModuleAuthority, ModuleCleanupAuthority};

fn cleanup(authority: ModuleAuthority) -> Result<(), (crate::XllError, ModuleCleanupAuthority)> {
    match authority.into_cleanup() {
        Ok(ready) => ready.finish(),
        Err(retained) => {
            let _ = retained.id();
            Ok(())
        }
    }
}
