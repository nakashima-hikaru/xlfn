// module: runtime
fn invalid(target: crate::module_runtime::ModuleExportsDrained, resources: super::CompletedResources<'_>) {
    let _ = target.certify(resources); // error: E0599
}
