// module: runtime
use crate::module_runtime::{ModuleAuthority, ModuleCleanupAuthority, UnavailableModuleAuthority};

fn unchecked_closing(authority: ModuleAuthority) {
    let _ = authority.into_closing(); // error: E0599
}

fn progressed_closing(authority: UnavailableModuleAuthority) {
    let _ = authority.into_closing(); // error: E0599
}

fn unchecked_cleanup(authority: ModuleCleanupAuthority) {
    let _ = authority.finish(); // error: E0599
}
