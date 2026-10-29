// module: runtime
use crate::module_runtime::ModuleExportsDrained;

fn invalid(exports: ModuleExportsDrained) {
    let _ = exports.certify(); // error: E0061
}
