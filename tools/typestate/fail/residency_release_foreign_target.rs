// module: runtime_components
fn invalid(target: &super::ModuleResidency, authority: &crate::runtime::ResidencyRelease<'_, ()>) {
    let _ = target.release_authorized(authority); // error: E0599
}
