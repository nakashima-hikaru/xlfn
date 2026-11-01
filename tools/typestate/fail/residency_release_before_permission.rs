// module: runtime
fn cannot_release_unqualified(module: &crate::runtime_components::ModuleResidency) {
    let _ = module;
    let _ = super::ModuleResidency::release_authorized(); // error: E0061
}
