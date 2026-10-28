// module: runtime
use crate::module_runtime::ModuleRetainedUndrained;

fn invalid(retained: ModuleRetainedUndrained) {
    let _ = retained.closing; // error: E0616
}
