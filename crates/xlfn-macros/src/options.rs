//! Attribute parsing and semantic option validation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, ExprLit, Lit, Meta, Token};

pub(super) fn parse_expr_path(expr: &Expr) -> syn::Result<syn::Path> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit_str),
            ..
        }) => lit_str.parse::<syn::Path>(),
        Expr::Path(expr_path) => Ok(expr_path.path.clone()),
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected a string literal or crate path",
        )),
    }
}

pub(super) fn resolve_crate_path(explicit_crate: Option<&syn::Path>) -> TokenStream {
    if let Some(path) = explicit_crate {
        return quote!(#path);
    }
    match proc_macro_crate::crate_name("xlfn") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(crate),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
            quote!(::#ident)
        }
        Err(_) => quote!(::xlfn),
    }
}

#[derive(Default)]
pub(super) struct FunctionOptions {
    pub(super) name: Option<String>,
    pub(super) id: Option<String>,
    pub(super) category: Option<String>,
    pub(super) description: Option<String>,
    pub(super) help_topic: Option<String>,
    pub(super) thread_safe: bool,
    pub(super) macro_sheet: bool,
    pub(super) volatile: bool,
    pub(super) hidden: bool,
    pub(super) krate: Option<syn::Path>,
}

#[derive(Default)]
pub(super) struct ArgumentOptions {
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) default: Option<Expr>,
    pub(super) blank: Option<String>,
    pub(super) missing: Option<String>,
    pub(super) reference: bool,
}

#[derive(Default)]
pub(super) struct AddinOptions {
    pub(super) name: Option<String>,
    pub(super) id: Option<String>,
    pub(super) category: Option<String>,
    pub(super) krate: Option<syn::Path>,
}

pub(super) fn gating_tokens(attributes: &[syn::Attribute]) -> TokenStream {
    let mut gating = Vec::new();
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            gating.push(quote!(#attribute));
            continue;
        }
        if !attribute.path().is_ident("cfg_attr") {
            continue;
        }

        let Ok(entries) =
            attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
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
            gating.push(quote!(#[cfg_attr(#predicate, #(#nested_gating),*)]));
        }
    }
    quote!(#(#gating)*)
}

#[derive(Clone, Copy)]
pub(super) enum ContextKind {
    ThreadSafe,
    MainThread,
    MacroSheet,
    Async,
}

pub(super) fn parse_context_attribute(attribute: &syn::Attribute) -> syn::Result<ContextKind> {
    let entries = attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    if entries.is_empty() {
        return Err(syn::Error::new_spanned(
            attribute,
            "#[excel_context(...)] requires one role: main_thread, thread_safe, macro_sheet, or asynchronous",
        ));
    }
    if entries.len() != 1 {
        return Err(syn::Error::new_spanned(
            entries,
            "#[excel_context(...)] accepts exactly one role",
        ));
    }
    let entry = entries.first().expect("one entry was validated");
    let Meta::Path(path) = entry else {
        return Err(syn::Error::new_spanned(entry, "expected a context role"));
    };
    if path.is_ident("main_thread") {
        Ok(ContextKind::MainThread)
    } else if path.is_ident("thread_safe") {
        Ok(ContextKind::ThreadSafe)
    } else if path.is_ident("macro_sheet") {
        Ok(ContextKind::MacroSheet)
    } else if path.is_ident("asynchronous") {
        Ok(ContextKind::Async)
    } else {
        Err(syn::Error::new_spanned(
            path,
            "unknown context role; expected main_thread, thread_safe, macro_sheet, or asynchronous",
        ))
    }
}

pub(super) fn parse_function_options(tokens: TokenStream) -> syn::Result<FunctionOptions> {
    let mut options = FunctionOptions::default();
    for meta in parse_meta(tokens)? {
        match meta {
            Meta::Path(path) if path.is_ident("thread_safe") => {
                if options.thread_safe {
                    return Err(syn::Error::new_spanned(path, "duplicate `thread_safe`"));
                }
                options.thread_safe = true;
            }
            Meta::Path(path) if path.is_ident("macro_sheet") => {
                if options.macro_sheet {
                    return Err(syn::Error::new_spanned(path, "duplicate `macro_sheet`"));
                }
                options.macro_sheet = true;
            }
            Meta::Path(path) if path.is_ident("volatile") => {
                if options.volatile {
                    return Err(syn::Error::new_spanned(path, "duplicate `volatile`"));
                }
                options.volatile = true;
            }
            Meta::Path(path) if path.is_ident("hidden") => {
                if options.hidden {
                    return Err(syn::Error::new_spanned(path, "duplicate `hidden`"));
                }
                options.hidden = true;
            }
            Meta::NameValue(value) if value.path.is_ident("name") => {
                if options.name.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `name`"));
                }
                options.name = Some(string_value(&value.value, "name")?);
            }
            Meta::NameValue(value) if value.path.is_ident("id") => {
                if options.id.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `id`"));
                }
                options.id = Some(string_value(&value.value, "id")?);
            }
            Meta::NameValue(value) if value.path.is_ident("category") => {
                if options.category.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `category`"));
                }
                options.category = Some(string_value(&value.value, "category")?);
            }
            Meta::NameValue(value) if value.path.is_ident("description") => {
                if options.description.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `description`"));
                }
                options.description = Some(string_value(&value.value, "description")?);
            }
            Meta::NameValue(value) if value.path.is_ident("help_topic") => {
                if options.help_topic.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `help_topic`"));
                }
                options.help_topic = Some(string_value(&value.value, "help_topic")?);
            }
            Meta::NameValue(value) if value.path.is_ident("crate") => {
                if options.krate.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `crate`"));
                }
                options.krate = Some(parse_expr_path(&value.value)?);
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `name`, `id`, `category`, `description`, `help_topic`, `thread_safe`, `macro_sheet`, `volatile`, `hidden`, or `crate`",
                ));
            }
        }
    }
    Ok(options)
}

pub(super) fn doc_comment(attributes: &[syn::Attribute]) -> String {
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

pub(super) fn parse_addin_options(tokens: TokenStream) -> syn::Result<AddinOptions> {
    let mut options = AddinOptions::default();
    for meta in parse_meta(tokens)? {
        match meta {
            Meta::NameValue(value) if value.path.is_ident("name") => {
                if options.name.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `name`"));
                }
                options.name = Some(string_value(&value.value, "name")?);
            }
            Meta::NameValue(value) if value.path.is_ident("id") => {
                if options.id.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `id`"));
                }
                options.id = Some(string_value(&value.value, "id")?);
            }
            Meta::NameValue(value) if value.path.is_ident("category") => {
                if options.category.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `category`"));
                }
                options.category = Some(string_value(&value.value, "category")?);
            }
            Meta::NameValue(value) if value.path.is_ident("crate") => {
                if options.krate.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate `crate`"));
                }
                options.krate = Some(parse_expr_path(&value.value)?);
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `name`, `id`, `category`, or `crate`",
                ));
            }
        }
    }
    Ok(options)
}

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

pub(super) fn parse_meta(tokens: TokenStream) -> syn::Result<Punctuated<Meta, Token![,]>> {
    Punctuated::<Meta, Token![,]>::parse_terminated.parse2(tokens)
}

pub(super) fn string_value(expression: &Expr, name: &str) -> syn::Result<String> {
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Ok(value.value()),
        _ => Err(syn::Error::new_spanned(
            expression,
            format!("`{name}` must be a string literal"),
        )),
    }
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
