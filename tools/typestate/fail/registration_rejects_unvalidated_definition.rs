// module: registration
fn invalid(
    host: &super::RegistrationHost<'_>,
    module: &super::host::CountedModuleName,
    descriptor: &super::RegistrationDescriptor,
) {
    let mut scratch = super::host::RegistrationScratch::default();
    let _ = host.register(module, descriptor, &mut scratch); // error: E0308
}
