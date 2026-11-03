// module: runtime
use crate::module_runtime::{ModuleCleanupAuthority, ModuleClosing};

fn drain(closing: ModuleClosing) -> ModuleCleanupAuthority {
    match closing.seal_and_drain() {
        Ok(drained) => ModuleCleanupAuthority::Drained(drained),
        Err((_, retained)) => {
            let _ = retained.id();
            ModuleCleanupAuthority::RetainedUndrained(retained)
        }
    }
}
