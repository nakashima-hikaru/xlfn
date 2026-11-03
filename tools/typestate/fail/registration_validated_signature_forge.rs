// module: registration
fn cannot_forge(raw: super::RegistrationSignature) {
    let _ = super::schema::ValidatedSignature(raw); // error: E0603
}
