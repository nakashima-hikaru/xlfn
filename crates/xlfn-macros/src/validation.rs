//! Semantic validation shared by the macro front ends.

pub(super) fn validate_addin_metadata(
    display_name: &str,
    id: &str,
    category: &str,
    span: &impl quote::ToTokens,
) -> syn::Result<()> {
    for (field, value) in [("name", display_name), ("category", category)] {
        let length = value.encode_utf16().count();
        if value.is_empty() || length > 255 {
            return Err(syn::Error::new_spanned(
                span,
                format!("add-in `{field}` must contain 1..=255 UTF-16 code units"),
            ));
        }
    }

    let valid_slug = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let upper = id.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
    if !valid_slug || reserved {
        return Err(syn::Error::new_spanned(
            span,
            "add-in `id` must be a non-reserved ASCII slug beginning with a letter and containing only letters, digits, `-`, or `_`",
        ));
    }
    Ok(())
}

pub(super) fn validate_export_id(id: &str, span: &impl quote::ToTokens) -> syn::Result<()> {
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
