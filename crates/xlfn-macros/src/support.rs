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
            let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
            syn::parse_quote!(::#ident)
        }
        Err(_) => syn::parse_quote!(::xlfn),
    }
}

pub(super) fn extract_gating_attributes(attributes: &[Attribute]) -> Vec<Attribute> {
    attributes
        .iter()
        .filter_map(|attribute| {
            let meta = gating_meta(&attribute.meta)?;
            Some(syn::parse_quote!(#[#meta]))
        })
        .collect()
}

fn gating_meta(meta: &Meta) -> Option<Meta> {
    if meta.path().is_ident("cfg") {
        return Some(meta.clone());
    }
    let Meta::List(list) = meta else {
        return None;
    };
    if !list.path.is_ident("cfg_attr") {
        return None;
    }
    let mut entries = list
        .parse_args_with(Punctuated::<Meta, syn::Token![,]>::parse_terminated)
        .ok()?
        .into_iter();
    let predicate = entries.next()?;
    let nested = entries
        .filter_map(|meta| gating_meta(&meta))
        .collect::<Vec<_>>();
    if nested.is_empty() {
        return None;
    }
    Some(syn::parse_quote!(cfg_attr(#predicate, #(#nested),*)))
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
    fn nested_cfg_attr_keeps_only_gating() {
        let attrs = vec![
            syn::parse_quote!(#[cfg_attr(all(), cfg_attr(all(), unsafe(export_name = "injected"), cfg(any())), inline)]),
        ];
        let result = extract_gating_attributes(&attrs);
        let text = quote::quote!(#(#result)*).to_string();
        assert!(!text.contains("export_name"));
        assert!(!text.contains("inline"));
        assert!(text.contains("cfg (any ())"));
    }
}
