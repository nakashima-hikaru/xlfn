// module: runtime
use super::CompletedResources;
use crate::module_runtime::ModuleExportsDrained;

fn invalid(resources: &mut CompletedResources<'_>, other: &ModuleExportsDrained) {
    resources.module = other; // error: E0616
}
