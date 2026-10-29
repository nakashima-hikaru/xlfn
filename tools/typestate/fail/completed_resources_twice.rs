// module: runtime
use super::CompletedResources;
use crate::module_runtime::ModuleExportsDrained;

fn invalid(first: ModuleExportsDrained, second: ModuleExportsDrained, resources: CompletedResources) {
    let _ = first.certify(resources);
    let _ = second.certify(resources); // error: E0382
}
