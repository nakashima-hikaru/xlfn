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
    let mut gating = Vec::new();
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            gating.push(attribute.clone());
            continue;
        }
        if !attribute.path().is_ident("cfg_attr") {
            continue;
        }

        let Ok(entries) =
            attribute.parse_args_with(Punctuated::<Meta, syn::Token![,]>::parse_terminated)
        else {
            continue;
        };
        let mut entries = entries.into_iter();
        let Some(predicate) = entries.next() else {
            continue;
        };
        let nested_gating = entries
            .filter(|meta| meta.path().is_ident("cfg") || meta.path().is_ident("cfg_attr"))
            .collect::<Vec<_>>();
        if !nested_gating.is_empty() {
            gating.push(syn::parse_quote!(#[cfg_attr(#predicate, #(#nested_gating),*)]));
        }
    }
    gating
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
