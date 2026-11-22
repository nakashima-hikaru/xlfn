//! Token-free helpers shared by the parser, semantic analysis, and emitters.

use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, ExprLit, Lit, Meta};

pub(super) fn resolve_crate_path(explicit_crate: Option<&syn::Path>) -> syn::Path {
    if let Some(path) = explicit_crate {
        return path.clone();
    }
    match proc_macro_crate::crate_name("xlfn") {
        Ok(proc_macro_crate::FoundCrate::Itself) => syn::parse_quote!(crate),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = dependency_ident(&name);
            syn::parse_quote!(::#ident)
        }
        Err(_) => syn::parse_quote!(::xlfn),
    }
}

fn dependency_ident(name: &str) -> syn::Ident {
    let span = proc_macro2::Span::call_site();
    // Cargo package aliases can be Rust keywords (e.g. "type"). Use
    // their raw spelling when Rust requires it, including edition keywords.
    match syn::parse_str::<syn::Ident>(name) {
        Ok(mut ident) if !matches!(name, "gen" | "macro_rules") => {
            ident.set_span(span);
            ident
        }
        _ => syn::Ident::new_raw(name, span),
    }
}

pub(super) fn extract_gating_attributes(attributes: &[Attribute]) -> syn::Result<Vec<Attribute>> {
    let mut gating = Vec::new();
    for attribute in attributes {
        if let Some(meta) = gating_meta(&attribute.meta)? {
            gating.push(syn::parse_quote!(#[#meta]));
        }
    }
    Ok(gating)
}

fn gating_meta(meta: &Meta) -> syn::Result<Option<Meta>> {
    if meta.path().is_ident("cfg") {
        return Ok(Some(meta.clone()));
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok(None);
    }
    let Meta::List(list) = meta else {
        return Err(syn::Error::new_spanned(
            meta,
            "expected cfg_attr(predicate, attributes...)",
        ));
    };
    let (predicate, entries) = list.parse_args_with(|input: syn::parse::ParseStream<'_>| {
        // Rust cfg predicates accept boolean literals as well as meta items.
        let predicate = if input.peek(syn::LitBool) {
            let literal: syn::LitBool = input.parse()?;
            quote::quote!(#literal)
        } else {
            let meta: Meta = input.parse()?;
            quote::quote!(#meta)
        };
        if input.is_empty() {
            return Ok((predicate, Punctuated::new()));
        }
        input.parse::<syn::Token![,]>()?;
        let entries = Punctuated::<Meta, syn::Token![,]>::parse_terminated(input)?;
        Ok((predicate, entries))
    })?;
    let mut nested = Vec::new();
    for entry in entries {
        if let Some(meta) = gating_meta(&entry)? {
            nested.push(meta);
        }
    }
    if nested.is_empty() {
        return Ok(None);
    }
    Ok(Some(syn::parse_quote!(cfg_attr(#predicate, #(#nested),*))))
}

pub(super) fn doc_comment(attributes: &[Attribute]) -> String {
    let lines = attributes
        .iter()
        .filter_map(|attribute| {
            if !attribute.path().is_ident("doc") {
                return None;
            }
            let Meta::NameValue(value) = &attribute.meta else {
                return None;
            };
            let Expr::Lit(ExprLit {
                lit: Lit::Str(value),
                ..
            }) = &value.value
            else {
                return None;
            };
            Some(value.value().trim().to_owned())
        })
        .collect::<Vec<_>>();
    lines.join("\n").trim().to_owned()
}

#[cfg(test)]
mod security_tests {
    use super::*;
    #[test]
    fn dependency_aliases_can_be_keywords() {
        for (name, spelling) in [("facade", "facade"), ("type", "r#type"), ("gen", "r#gen")] {
            let ident = dependency_ident(name);
            assert_eq!(ident.to_string(), spelling);
            assert!(syn::parse_str::<syn::Path>(&format!("::{ident}::value")).is_ok());
        }
    }

    #[test]
    fn malformed_gating_attribute_is_not_silently_discarded() {
        let malformed = syn::parse_quote!(#[cfg_attr(3, cfg(any()))]);
        assert!(extract_gating_attributes(&[malformed]).is_err());
    }

    #[test]
    fn nested_cfg_attr_keeps_only_gating() {
        let attrs = vec![
            syn::parse_quote!(#[cfg_attr(true, cfg_attr(false, unsafe(export_name = "injected"), cfg(any())), inline)]),
        ];
        let result = extract_gating_attributes(&attrs).unwrap();
        let text = quote::quote!(#(#result)*).to_string();
        assert!(!text.contains("export_name"));
        assert!(!text.contains("inline"));
        assert!(text.contains("cfg_attr (true , cfg_attr (false , cfg (any ())))"));
    }
}
