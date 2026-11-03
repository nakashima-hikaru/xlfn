// module: registration
fn cannot_set_unchecked(prepared: &mut super::preflight::PreparedRegistration, raw: super::RegistrationSignature) {
    prepared.signature = raw; // error: E0308
}
