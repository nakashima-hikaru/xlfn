// module: runtime
use crate::module_runtime::ModuleClosing;

fn invalid(closing: ModuleClosing) {
    if let Err((_, retained)) = closing.seal_and_drain() {
        let _ = retained.seal_and_drain(); // error: E0599
    }
}
