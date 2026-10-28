// module: runtime
use crate::module_runtime::{ModuleAuthority, ModuleCleanupAuthority};

fn invalid(authority: ModuleAuthority, cleanup: ModuleCleanupAuthority) {
    if let ModuleAuthority::RetainedUndrained(retained) = authority {
        let _ = retained.seal_and_drain(); // error: E0599
    }
    if let ModuleCleanupAuthority::RetainedUndrained(retained) = cleanup {
        let _ = retained.seal_and_drain(); // error: E0599
    }
}
