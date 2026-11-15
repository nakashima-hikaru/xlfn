// module: registration
fn invalid(host: &super::RegistrationHost<'_>, descriptor: &super::RegistrationDescriptor) {
    let _ = host.register(&[], descriptor); // error: E0308
}
