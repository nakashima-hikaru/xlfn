// module: registration
fn validates_before_encoding(raw: super::RegistrationSignature) -> crate::XllResult<String> {
    let checked = super::schema::ValidatedSignature::try_from(raw)?;
    Ok(checked.encode())
}
