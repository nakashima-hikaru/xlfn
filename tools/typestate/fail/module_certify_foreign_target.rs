// module: runtime
fn invalid(target: crate::module_runtime::ModuleExportsDrained, resources: super::CompletedResources) {
    let _ = target.certify(resources); // error: E0599
}
