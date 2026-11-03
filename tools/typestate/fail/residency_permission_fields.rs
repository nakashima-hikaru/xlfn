// module: runtime
fn cannot_retarget<'a, A: crate::Addin>(
    mut permission: super::residency::ResidencyRelease<'a, A>,
    module: &'a crate::runtime_components::ModuleResidency,
) {
    permission.module = module; // error: E0616
}
