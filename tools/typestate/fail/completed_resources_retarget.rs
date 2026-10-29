// module: runtime
use super::CompletedResources;
use crate::module_runtime::ModuleEpochId;

fn invalid(resources: &mut CompletedResources, other: ModuleEpochId) {
    resources.module_epoch = other; // error: E0616
}
