// module: registration
fn cannot_set_unchecked(prepared: &mut super::preflight::ValidatedRegistration, raw: super::RegistrationSignature) {
    prepared.signature = raw; // error: E0616
}
