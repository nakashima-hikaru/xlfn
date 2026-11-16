// module: registration
fn invalid(
    host: &super::RegistrationHost<'_>,
    module: &super::host::CountedModuleName,
    descriptor: &super::RegistrationDescriptor,
) {
    let _ = host.register(module, descriptor); // error: E0308
}
