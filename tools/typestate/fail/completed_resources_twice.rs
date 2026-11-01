// module: runtime
use super::CompletedResources;
use crate::module_runtime::ModuleExportsDrained;

fn invalid(first: ModuleExportsDrained, second: ModuleExportsDrained, resources: CompletedResources) {
    let _ = (first, second);
    let _ = crate::module_runtime::ModuleExportsDrained::certify(resources);
    let _ = crate::module_runtime::ModuleExportsDrained::certify(resources); // error: E0382
}
