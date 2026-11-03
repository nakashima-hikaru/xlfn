// module: runtime_components
fn ordered(authority: &crate::runtime::ResidencyRelease<'_, ()>) {
    let _outcome = super::ModuleResidency::release_authorized(authority);
}
