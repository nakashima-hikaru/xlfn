// module: registration
fn invalid(
    host: &super::RegistrationHost<'_>,
    module: &super::host::CountedModuleName,
    descriptor: &super::RegistrationDescriptor,
) {
    let mut text = super::host::RegistrationTextCache::default();
    let _ = host.register(module, descriptor, &mut text); // error: E0308
}
