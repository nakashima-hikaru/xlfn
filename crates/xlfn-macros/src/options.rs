//! Raw attribute parsing for the procedural macro front end.

use proc_macro2::TokenStream;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, ExprLit, Lit, Meta, Token};

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

#[derive(Default)]
pub(super) struct ParsedFunctionOptions {
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
pub(super) struct ParsedArgumentOptions {
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) default: Option<Expr>,
    pub(super) blank: Option<String>,
    pub(super) missing: Option<String>,
    pub(super) reference: bool,
}

#[derive(Default)]
pub(super) struct ParsedAddinOptions {
    pub(super) name: Option<String>,
    pub(super) id: Option<String>,
    pub(super) category: Option<String>,
    pub(super) krate: Option<syn::Path>,
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

/// Parses one `#[excel_arg(...)]` attribute without interpreting its policy
/// values. The parser only owns syntax concerns such as duplicate keys and
/// literal kinds; semantic normalization belongs to the UDF analyzer.
pub(super) fn parse_argument_options(
    attribute: &Attribute,
    options: &mut ParsedArgumentOptions,
) -> syn::Result<()> {
    for entry in attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)? {
        match entry {
            Meta::NameValue(value) if value.path.is_ident("name") => {
                if options.name.is_some() {
                    return Err(syn::Error::new_spanned(value, "duplicate excel_arg name"));
                }
                options.name = Some(string_value(&value.value, "name")?);
            }
            Meta::NameValue(value) if value.path.is_ident("description") => {
                if options.description.is_some() {
                    return Err(syn::Error::new_spanned(
                        value,
                        "duplicate excel_arg description",
                    ));
                }
                options.description = Some(string_value(&value.value, "description")?);
            }
            Meta::NameValue(value) if value.path.is_ident("default") => {
                if options.default.is_some() {
                    return Err(syn::Error::new_spanned(
                        value,
                        "duplicate excel_arg default",
                    ));
                }
                options.default = Some(value.value);
            }
            Meta::NameValue(value) if value.path.is_ident("blank") => {
                if options.blank.is_some() {
                    return Err(syn::Error::new_spanned(
                        value,
                        "duplicate excel_arg blank policy",
                    ));
                }
                options.blank = Some(string_value(&value.value, "blank")?);
            }
            Meta::NameValue(value) if value.path.is_ident("missing") => {
                if options.missing.is_some() {
                    return Err(syn::Error::new_spanned(
                        value,
                        "duplicate excel_arg missing policy",
                    ));
                }
                options.missing = Some(string_value(&value.value, "missing")?);
            }
            Meta::Path(path) if path.is_ident("reference") => {
                if options.reference {
                    return Err(syn::Error::new_spanned(path, "duplicate `reference`"));
                }
                options.reference = true;
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `name`, `description`, `default`, `blank`, `missing`, or `reference`",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn parse_function_options(tokens: TokenStream) -> syn::Result<ParsedFunctionOptions> {
    let mut options = ParsedFunctionOptions::default();
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

pub(super) fn parse_addin_options(tokens: TokenStream) -> syn::Result<ParsedAddinOptions> {
    let mut options = ParsedAddinOptions::default();
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
