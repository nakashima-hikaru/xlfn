//! Semantic validation shared by the macro front ends.

pub(super) fn validate_registration_string(
    field: &str,
    value: &str,
    span: &impl quote::ToTokens,
) -> syn::Result<()> {
    xlfn_common::validate_excel_string(value)
        .map_err(|error| syn::Error::new_spanned(span, format!("invalid {field}: {error}")))
}

pub(super) struct ValidatedAddinMetadata {
    name_units: Vec<u16>,
    name_length: u8,
}

impl ValidatedAddinMetadata {
    pub(super) fn name(&self) -> (u16, &[u16]) {
        (u16::from(self.name_length), &self.name_units)
    }
}

fn validate_addin_text(field: &str, value: &str, span: &impl quote::ToTokens) -> syn::Result<u8> {
    let length = u8::try_from(value.encode_utf16().count())
        .ok()
        .filter(|&length| length != 0)
        .ok_or_else(|| {
            syn::Error::new_spanned(
                span,
                format!("add-in `{field}` must contain 1..=255 UTF-16 code units"),
            )
        })?;
    validate_registration_string(&format!("add-in `{field}`"), value, span)?;
    Ok(length)
}

pub(super) fn validate_addin_metadata(
    display_name: &str,
    id: &str,
    category: &str,
    span: &impl quote::ToTokens,
) -> syn::Result<ValidatedAddinMetadata> {
    let name_length = validate_addin_text("name", display_name, span)?;
    validate_addin_text("category", category, span)?;

    let valid_slug = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid_slug || xlfn_common::validate_windows_basename(id).is_err() {
        return Err(syn::Error::new_spanned(
            span,
            "add-in `id` must be a non-reserved ASCII slug beginning with a letter and containing only letters, digits, `-`, or `_`",
        ));
    }
    Ok(ValidatedAddinMetadata {
        name_units: display_name.encode_utf16().collect(),
        name_length,
    })
}

pub(super) fn validate_export_id(id: &str, span: &impl quote::ToTokens) -> syn::Result<()> {
    if id.len() > 251 {
        return Err(syn::Error::new_spanned(
            span,
            "`xll_` plus `id` must fit 255 ASCII bytes",
        ));
    }
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
        || id.starts_with(|character: char| character.is_ascii_digit())
    {
        return Err(syn::Error::new_spanned(
            span,
            "`id` must be a Rust identifier fragment",
        ));
    }
    Ok(())
}
