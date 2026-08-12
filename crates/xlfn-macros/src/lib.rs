//! Attribute and derive macros for the public `xlfn` API.
//!
//! The macros generate the Excel ABI entry points, registration metadata, and
//! add-in lifecycle exports consumed by `xlfn`. They intentionally keep raw
//! pointer handling at the generated FFI boundary; user functions receive the
//! safe values and contexts exposed by `xlfn-core`.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use std::collections::BTreeSet;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{
    Data, DeriveInput, Expr, ExprLit, Fields, FnArg, ItemFn, ItemStruct, Lit, Meta, Pat,
    ReturnType, Token, parse_macro_input,
};

fn parse_expr_path(expr: &Expr) -> syn::Result<syn::Path> {
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

fn resolve_crate_path(explicit_crate: Option<&syn::Path>) -> proc_macro2::TokenStream {
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
struct FunctionOptions {
    name: Option<String>,
    id: Option<String>,
    category: Option<String>,
    description: Option<String>,
    help_topic: Option<String>,
    thread_safe: bool,
    macro_sheet: bool,
    volatile: bool,
    hidden: bool,
    krate: Option<syn::Path>,
}

#[derive(Default)]
struct ArgumentOptions {
    name: Option<String>,
    description: Option<String>,
    default: Option<Expr>,
    blank: Option<String>,
    missing: Option<String>,
    reference: bool,
}

#[derive(Default)]
struct AddinOptions {
    name: Option<String>,
    id: Option<String>,
    category: Option<String>,
    krate: Option<syn::Path>,
}

fn gating_tokens(attributes: &[syn::Attribute]) -> proc_macro2::TokenStream {
    let mut gating = Vec::new();
    for attribute in attributes {
        if attribute.path().is_ident("cfg") {
            gating.push(quote!(#attribute));
            continue;
        }
        if !attribute.path().is_ident("cfg_attr") {
            continue;
        }

        // Only propagate the gating portion of `cfg_attr`. Copying the whole
        // attribute could apply item-specific attributes such as `derive` or
        // `allow` to generated functions, statics, or impl blocks where they
        // are invalid or change unrelated diagnostics.
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

/// Attributes a function as an Excel UDF.
///
/// Excel-visible arguments and return values are selected by their conversion
/// trait implementations. Injected contexts must be the first parameter and
/// carry an explicit `#[excel_context(...)]` role.
#[proc_macro_attribute]
pub fn excel_function(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    match expand_excel_function(attributes.into(), function) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

/// Defines the one Add-in for an XLL crate and emits its lifecycle exports.
///
/// This attribute must be placed on a struct declared at the crate root.
#[proc_macro_attribute]
pub fn excel_addin(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemStruct);
    match expand_excel_addin(attributes.into(), item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_derive(ExcelHandleObject, attributes(excel_handle))]
pub fn derive_excel_handle_object(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match expand_excel_handle_object(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_derive(ExcelEnum, attributes(excel_enum, excel_value))]
pub fn derive_excel_enum(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match expand_excel_enum(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand_excel_enum(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let mut ascii_case_insensitive = false;
    let mut krate_opt = None;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("excel_enum") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("ascii_case_insensitive") {
                if ascii_case_insensitive {
                    return Err(meta.error("duplicate ASCII case-insensitive option"));
                }
                ascii_case_insensitive = true;
                Ok(())
            } else if meta.path.is_ident("crate") {
                if krate_opt.is_some() {
                    return Err(meta.error("duplicate `crate` option"));
                }
                let expr: Expr = meta.value()?.parse()?;
                krate_opt = Some(parse_expr_path(&expr)?);
                Ok(())
            } else {
                Err(meta.error("expected `ascii_case_insensitive` or `crate = \"...\"`"))
            }
        })?;
    }
    let krate = resolve_crate_path(krate_opt.as_ref());
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "ExcelEnum can only be derived for an enum",
        ));
    };
    let mut names = BTreeSet::new();
    let mut variants = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                &variant.fields,
                "ExcelEnum variants must not contain fields",
            ));
        }
        let mut excel_name = None;
        for attribute in &variant.attrs {
            if !attribute.path().is_ident("excel_value") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    if excel_name.is_some() {
                        return Err(meta.error("duplicate `name`"));
                    }
                    excel_name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                    Ok(())
                } else {
                    Err(meta.error("expected `name = \"...\"`"))
                }
            })?;
        }
        let excel_name = excel_name.unwrap_or_else(|| variant.ident.to_string());
        if excel_name.is_empty() {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                "Excel enum value cannot be empty",
            ));
        }
        let uniqueness_key = if ascii_case_insensitive {
            excel_name.to_ascii_lowercase()
        } else {
            excel_name.clone()
        };
        if !names.insert(uniqueness_key) {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                "duplicate Excel enum value",
            ));
        }
        variants.push((variant.ident.clone(), excel_name));
    }
    let ident = &input.ident;
    let (base_impl_generics, type_generics, base_where_clause) = input.generics.split_for_impl();
    let mut from_excel_generics = input.generics.clone();
    from_excel_generics
        .params
        .insert(0, syn::parse_quote!('__xlfn_call));
    let (from_excel_impl_generics, _, from_excel_where_clause) =
        from_excel_generics.split_for_impl();
    let comparisons = variants.iter().map(|(variant, name)| {
        let units = name
            .encode_utf16()
            .map(|unit| quote!(#unit))
            .collect::<Vec<_>>();
        if ascii_case_insensitive {
            quote! {
                if #krate::__private::utf16_eq_ignore_ascii_case(
                    __text.as_utf16(),
                    &[#(#units),*],
                ) {
                    return ::core::result::Result::Ok(Self::#variant);
                }
            }
        } else {
            quote! {
                if __text.as_utf16() == &[#(#units),*] {
                    return ::core::result::Result::Ok(Self::#variant);
                }
            }
        }
    });
    let outputs = variants
        .iter()
        .map(|(variant, name)| quote!(Self::#variant => #name))
        .collect::<Vec<_>>();
    Ok(quote! {
        impl #from_excel_impl_generics #krate::convert::FromExcel<'__xlfn_call>
            for #ident #type_generics #from_excel_where_clause
        {
            fn from_excel(
                __value: #krate::convert::XlValueRef<'__xlfn_call>,
                __argument: &'static str,
                __context: &#krate::convert::CallContext,
            ) -> #krate::error::XllResult<Self> {
                let __text = __value.as_str_with_argument(__argument)?;
                #(#comparisons)*
                if __text.chars().any(|__decoded| __decoded.is_err()) {
                    ::core::result::Result::Err(#krate::error::XllError::input(
                        __argument,
                        #krate::error::InputError::InvalidUtf16,
                    ))
                } else {
                    ::core::result::Result::Err(#krate::error::XllError::input(
                        __argument,
                        #krate::error::InputError::Malformed("unknown enum value"),
                    ))
                }
            }
        }

        impl #base_impl_generics #krate::convert::IntoExcelValue
            for #ident #type_generics #base_where_clause
        {
            fn into_excel_value(
                self,
            ) -> #krate::error::XllResult<#krate::convert::OwnedExcelValue> {
                let __text = match self {
                    #(#outputs,)*
                };
                <&str as #krate::convert::IntoExcelValue>::into_excel_value(__text)
            }
        }

        impl #base_impl_generics #krate::convert::ExcelReturn
            for #ident #type_generics #base_where_clause
        {
            type Output = Self;

            fn into_excel(
                self,
                _: &mut #krate::__private::ReturnContext<'_, '_>,
            ) -> #krate::error::XllResult<Self::Output> {
                ::core::result::Result::Ok(self)
            }
        }

        impl #base_impl_generics #krate::convert::MainThreadReturn
            for #ident #type_generics #base_where_clause
        {}
        impl #base_impl_generics #krate::convert::ThreadSafeReturn
            for #ident #type_generics #base_where_clause
        {}
        impl #base_impl_generics #krate::convert::MacroSheetReturn
            for #ident #type_generics #base_where_clause
        {}
        impl #base_impl_generics #krate::convert::AsyncReturn
            for #ident #type_generics #base_where_clause
        {}
        impl #base_impl_generics #krate::convert::VolatileReturn
            for #ident #type_generics #base_where_clause
        {}

    })
}

fn expand_excel_handle_object(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let mut krate_opt = None;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("excel_handle") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                if krate_opt.is_some() {
                    return Err(meta.error("duplicate `crate` option"));
                }
                let expr: Expr = meta.value()?.parse()?;
                krate_opt = Some(parse_expr_path(&expr)?);
                Ok(())
            } else {
                Err(meta.error("expected `crate = \"...\"`"))
            }
        })?;
    }
    let krate = resolve_crate_path(krate_opt.as_ref());
    let ident = input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::handle::ExcelHandleObject
            for #ident #type_generics #where_clause
        {}

        impl #impl_generics #krate::convert::ExcelReturn
            for #ident #type_generics #where_clause
        {
            type Output = ::std::string::String;

            fn invoke(
                __context: &mut #krate::__private::ReturnContext<'_, '_>,
                __operation: impl ::core::ops::FnOnce()
                    -> #krate::error::XllResult<Self>,
            ) -> #krate::error::XllResult<Self::Output> {
                __context.publish_new_handle(__operation)
            }

            fn into_excel(
                self,
                __context: &mut #krate::__private::ReturnContext<'_, '_>,
            ) -> #krate::error::XllResult<Self::Output> {
                __context.publish_new_handle(|| ::core::result::Result::Ok(self))
            }
        }

        impl #impl_generics #krate::convert::MainThreadReturn
            for #ident #type_generics #where_clause
        {}
    })
}

fn expand_excel_function(
    attributes: proc_macro2::TokenStream,
    mut function: ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let options = parse_function_options(attributes)?;
    let krate = resolve_crate_path(options.krate.as_ref());
    let gating = gating_tokens(&function.attrs);
    let is_async = function.sig.asyncness.is_some();
    let return_type = match &function.sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => ty.as_ref().clone(),
    };
    if function.sig.constness.is_some()
        || matches!(function.sig.safety, syn::Safety::Unsafe(_))
        || function.sig.abi.is_some()
    {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "Excel functions must be ordinary safe Rust functions",
        ));
    }
    if !function.sig.generics.params.is_empty() || function.sig.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &function.sig.generics,
            "Excel functions cannot be generic",
        ));
    }
    if function.sig.variadic.is_some() {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "Excel functions cannot be variadic",
        ));
    }

    let function_ident = &function.sig.ident;
    let udf_id = options.id.unwrap_or_else(|| function_ident.to_string());
    validate_export_id(&udf_id, function_ident)?;
    let excel_name = options.name.unwrap_or_else(|| function_ident.to_string());
    if excel_name.trim().is_empty() {
        return Err(syn::Error::new_spanned(
            function_ident,
            "Excel function name cannot be empty",
        ));
    }
    let category = options.category.unwrap_or_default();
    let description = options
        .description
        .unwrap_or_else(|| doc_comment(&function.attrs));
    let help_topic = options.help_topic.unwrap_or_default();
    let visibility = if options.hidden {
        quote!(#krate::__private::FunctionVisibility::Hidden)
    } else {
        quote!(#krate::__private::FunctionVisibility::Public)
    };
    let export_ident = format_ident!("xll_{}", udf_id);
    let descriptor_ident = format_ident!("__XLFN_DESCRIPTOR_{}", udf_id);
    let export_directive_ident = format_ident!("__XLFN_EXPORT_{}", udf_id);
    let export_manifest_entry_ident = format_ident!("__XLFN_EXP_{}", udf_id);
    let export_name_nul = format!("xll_{udf_id}\0");
    let export_name_bytes =
        syn::LitByteStr::new(export_name_nul.as_bytes(), proc_macro2::Span::call_site());

    let mut context = None;
    let mut context_type = None;
    for (index, input) in function.sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = input else {
            continue;
        };
        let mut retained = Vec::new();
        let mut argument_context = None;
        let mut has_excel_arg = false;
        for attribute in std::mem::take(&mut argument.attrs) {
            if attribute.path().is_ident("excel_context") {
                let kind = parse_context_attribute(&attribute)?;
                if argument_context.replace(kind).is_some() {
                    return Err(syn::Error::new_spanned(
                        attribute,
                        "an argument can have only one #[excel_context(...)] role",
                    ));
                }
            } else {
                has_excel_arg |= attribute.path().is_ident("excel_arg");
                retained.push(attribute);
            }
        }
        argument.attrs = retained;
        if let Some(kind) = argument_context {
            if context.is_some() {
                return Err(syn::Error::new_spanned(
                    argument,
                    "only one #[excel_context(...)] parameter is allowed",
                ));
            }
            if index != 0 {
                return Err(syn::Error::new_spanned(
                    argument,
                    "the #[excel_context(...)] parameter must be the first argument",
                ));
            }
            if has_excel_arg {
                return Err(syn::Error::new_spanned(
                    argument,
                    "#[excel_context(...)] cannot be combined with #[excel_arg(...)]",
                ));
            }
            context = Some(kind);
            context_type = Some(argument.ty.as_ref().clone());
        }
    }
    if is_async && context.is_some() && !matches!(context, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an async Excel function must use #[excel_context(asynchronous)]",
        ));
    }
    if !is_async && matches!(context, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "#[excel_context(asynchronous)] can only be used by an async Excel function",
        ));
    }
    if matches!(context, Some(ContextKind::MainThread)) && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "a main-thread context function cannot be marked `thread_safe`",
        ));
    }
    if matches!(context, Some(ContextKind::MacroSheet)) && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "a macro-sheet context function cannot be marked `thread_safe`",
        ));
    }
    if options.macro_sheet && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "a macro-sheet function cannot be marked `thread_safe`",
        ));
    }
    if options.macro_sheet && is_async {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "an async Excel function cannot be a macro-sheet function",
        ));
    }
    if options.macro_sheet
        && matches!(
            context,
            Some(ContextKind::MainThread | ContextKind::ThreadSafe | ContextKind::Async)
        )
    {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "`macro_sheet` is incompatible with this context role",
        ));
    }
    let skip = usize::from(context.is_some());
    let mut argument_options = Vec::new();
    for input in function.sig.inputs.iter_mut().skip(skip) {
        let FnArg::Typed(argument) = input else {
            continue;
        };
        let mut options = ArgumentOptions::default();
        let mut retained = Vec::new();
        for attribute in std::mem::take(&mut argument.attrs) {
            if attribute.path().is_ident("excel_arg") {
                for entry in
                    attribute.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?
                {
                    match entry {
                        Meta::NameValue(value) if value.path.is_ident("name") => {
                            if options.name.is_some() {
                                return Err(syn::Error::new_spanned(
                                    value,
                                    "duplicate excel_arg name",
                                ));
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
            } else {
                retained.push(attribute);
            }
        }
        argument.attrs = retained;
        for (kind, policy) in [
            ("blank", options.blank.as_deref()),
            ("missing", options.missing.as_deref()),
        ] {
            if let Some(policy) = policy {
                if !matches!(policy, "default" | "error") {
                    return Err(syn::Error::new_spanned(
                        argument,
                        format!("`{kind}` must be \"default\" or \"error\""),
                    ));
                }
                if policy == "default" && options.default.is_none() {
                    return Err(syn::Error::new_spanned(
                        argument,
                        format!("`{kind} = \"default\"` requires `default = ...`"),
                    ));
                }
            }
        }
        if options.default.is_some()
            && options.blank.as_deref() != Some("default")
            && options.missing.as_deref() != Some("default")
        {
            return Err(syn::Error::new_spanned(
                argument,
                "`default = ...` requires `blank = \"default\"` or `missing = \"default\"`",
            ));
        }
        if options.reference
            && (options.default.is_some() || options.blank.is_some() || options.missing.is_some())
        {
            return Err(syn::Error::new_spanned(
                argument,
                "reference arguments cannot use blank, missing, or default policies",
            ));
        }
        argument_options.push(options);
    }
    let typed_inputs = function
        .sig
        .inputs
        .iter()
        .skip(skip)
        .map(|argument| match argument {
            FnArg::Typed(argument) => Ok(argument),
            FnArg::Receiver(receiver) => Err(syn::Error::new_spanned(
                receiver,
                "Excel functions must be free functions",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    let maximum_visible = if is_async { 254 } else { 255 };
    if typed_inputs.len() > maximum_visible {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!("Excel functions support at most {maximum_visible} arguments"),
        ));
    }

    let mut argument_names = Vec::with_capacity(typed_inputs.len());
    let mut argument_types = Vec::with_capacity(typed_inputs.len());
    for argument in &typed_inputs {
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &argument.pat,
                "Excel function arguments must use simple identifier patterns",
            ));
        };
        argument_names.push(pattern.ident.clone());
        argument_types.push(argument.ty.as_ref().clone());
    }
    let raw_names = (0..argument_names.len())
        .map(|index| format_ident!("__raw_{index}"))
        .collect::<Vec<_>>();
    let converted_names = (0..argument_names.len())
        .map(|index| format_ident!("__argument_{index}"))
        .collect::<Vec<_>>();
    let argument_name_literals = argument_names
        .iter()
        .zip(&argument_options)
        .map(|(name, options)| options.name.clone().unwrap_or_else(|| name.to_string()))
        .collect::<Vec<_>>();
    for name in &argument_name_literals {
        let utf16_len = name.encode_utf16().count();
        if name.is_empty() || name.contains([',', '\0', '\r', '\n']) || utf16_len > 32_767 {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "Excel argument names must be non-empty counted strings without comma, NUL, CR, or LF",
            ));
        }
    }
    let joined_argument_name_len = argument_name_literals
        .iter()
        .map(|name| name.encode_utf16().count())
        .sum::<usize>()
        .saturating_add(argument_name_literals.len().saturating_sub(1));
    if joined_argument_name_len > 32_767 {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "combined Excel argument names exceed the 32,767 UTF-16 unit counted-string limit",
        ));
    }
    let argument_descriptions = argument_options
        .iter()
        .map(|options| options.description.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    let reference_arguments = argument_options
        .iter()
        .map(|options| options.reference)
        .collect::<Vec<_>>();
    let abi_argument_count = argument_names.len() + usize::from(is_async);
    let x86_export_directive = format!(
        " /EXPORT:{0}=_{0}@{1}",
        export_ident,
        abi_argument_count * 4
    );
    let x86_export_directive = syn::LitByteStr::new(
        x86_export_directive.as_bytes(),
        proc_macro2::Span::call_site(),
    );
    let generated_context_expression = context.map(|kind| match kind {
        ContextKind::ThreadSafe => {
            quote!(#krate::context::ThreadSafeContext::new(__state))
        }
        ContextKind::MainThread => quote!(#krate::context::MainThreadContext::new(
            __state,
            &crate::__XLFN_RUNTIME,
            __call_scope,
        )),
        ContextKind::MacroSheet => {
            quote!(#krate::context::MacroSheetContext::new(
                __state,
                __call_scope
            ))
        }
        ContextKind::Async => {
            quote!(#krate::context::AsyncContext::new(__state, __cancellation))
        }
    });
    let macro_sheet = options.macro_sheet || matches!(context, Some(ContextKind::MacroSheet));
    let thread_safe =
        is_async || matches!(context, Some(ContextKind::ThreadSafe)) || options.thread_safe;
    if is_async && reference_arguments.iter().any(|reference| *reference) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an async Excel function cannot accept reference arguments",
        ));
    }
    if reference_arguments.iter().any(|reference| *reference) && !macro_sheet {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "a reference argument requires `macro_sheet` or MacroSheetContext",
        ));
    }
    let invocation = if generated_context_expression.is_some() {
        quote!(#function_ident(__context, #(#converted_names),*))
    } else {
        quote!(#function_ident(#(#converted_names),*))
    };
    let async_result_expression = quote! {
        let __result = #invocation.await;
        let mut __return_context =
            #krate::__private::ReturnContext::new();
        #krate::convert::ExcelReturn::invoke(
            &mut __return_context,
            || ::core::result::Result::Ok(__result),
        )
    };
    let context_setup = generated_context_expression.map(|expression| {
        let ty = context_type
            .as_ref()
            .expect("context expression always has a declared type");
        quote! {
            let __generated_context = #expression;
            let __context: #ty = __generated_context;
        }
    });
    let argument_abis = reference_arguments.iter().map(|reference| {
        if *reference {
            quote!(#krate::__private::ArgumentAbi::RawReference)
        } else {
            quote!(#krate::__private::ArgumentAbi::CoercedValue)
        }
    });
    let volatile = options.volatile;
    let mode_assertion = if is_async {
        quote!(#krate::__private::assert_async_return::<#return_type>();)
    } else if macro_sheet {
        quote!(#krate::__private::assert_macro_sheet_return::<#return_type>();)
    } else if thread_safe {
        quote!(#krate::__private::assert_thread_safe_return::<#return_type>();)
    } else {
        quote!(#krate::__private::assert_main_thread_return::<#return_type>();)
    };
    let volatile_assertion =
        volatile.then(|| quote!(#krate::__private::assert_volatile_return::<#return_type>();));
    let return_assertion = quote! {
        #mode_assertion
        #volatile_assertion
    };
    let conversions = converted_names
        .iter()
        .zip(argument_types.iter())
        .zip(argument_name_literals.iter())
        .zip(raw_names.iter())
        .zip(argument_options.iter())
        .map(|((((converted, ty), argument), raw), options)| {
            let conversion = if options.reference {
                quote! {
                    // SAFETY: Excel supplies the live reference pointer and
                    // raw argument slot for this ABI call.
                    unsafe {
                        #krate::__private::reference_from_raw(#argument, #raw)
                    }
                }
            } else {
                let async_assertion = is_async.then(|| {
                    quote!(#krate::__private::assert_async_parameter::<#ty>();)
                });
                quote! {
                    {
                        #async_assertion
                        #krate::__private::assert_excel_parameter::<#ty>(__call_scope);
                        // SAFETY: Excel supplies the live XLOPER12 pointer and
                        // raw argument slot for this ABI call.
                        unsafe {
                            #krate::__private::argument_from_raw_with_context(
                                __call_scope,
                                &crate::__XLFN_RUNTIME,
                                #argument,
                                #raw,
                            )
                        }
                    }
                }
            };
            let blank_default = options.blank.as_deref() == Some("default");
            let missing_default = options.missing.as_deref() == Some("default");
            let blank_error = options.blank.as_deref() == Some("error");
            let missing_error = options.missing.as_deref() == Some("error");
            if blank_default || missing_default || blank_error || missing_error {
                let default_expr = options.default.as_ref();
                let blank_arm = if blank_default {
                    let default = default_expr.expect("validated default policy has an expression");
                    quote!(#krate::convert::CellPresence::Blank => #default,)
                } else if blank_error {
                    quote!(#krate::convert::CellPresence::Blank => return ::core::result::Result::Err(#krate::error::XllError::input(#argument, #krate::error::InputError::Malformed("blank cell is not allowed"))),)
                } else {
                    quote!()
                };
                let missing_arm = if missing_default {
                    let default = default_expr.expect("validated default policy has an expression");
                    quote!(#krate::convert::CellPresence::Missing => #default,)
                } else if missing_error {
                    quote!(#krate::convert::CellPresence::Missing => return ::core::result::Result::Err(#krate::error::XllError::input(#argument, #krate::error::InputError::Malformed("missing argument is not allowed"))),)
                } else {
                    quote!()
                };
                quote! {
                    // SAFETY: the raw argument belongs to the current Excel
                    // call and is validated by the conversion boundary.
                    let #converted: #ty = match unsafe {
                        #krate::__private::cell_presence_from_raw(#argument, #raw)
                    }? {
                        #blank_arm
                        #missing_arm
                        _ => #conversion?,
                    };
                }
            } else {
                quote! {
                    let #converted: #ty = #conversion?;
                }
            }
        })
        .collect::<Vec<_>>();

    let boundary = if is_async {
        quote! {
            #krate::__xlfn_async_only! {
                // SAFETY: `__async_handle` is provided by Excel via the extern "system"
                // entry point generated by this macro and points to a valid async handle.
                unsafe {
                    #krate::__private::async_udf_boundary_named(
                        &crate::__XLFN_RUNTIME,
                        #udf_id,
                        #excel_name,
                        __async_handle,
                        |__state, __cancellation| {
                            #krate::__private::with_excel_call_scope(|__call_scope| {
                                #context_setup
                                #(#conversions)*
                                ::core::result::Result::Ok(async move {
                                    #return_assertion
                                    #async_result_expression
                                })
                            })
                        },
                    )
                }
            }
        }
    } else {
        let raw_argument_count = raw_names.len();
        quote! {
            #krate::__private::udf_boundary_named(
                &crate::__XLFN_RUNTIME,
                #udf_id,
                #excel_name,
                |__state| {
                    #return_assertion
                    let __raw_arguments:
                        [*mut #krate::sys::XLOPER12; #raw_argument_count] =
                        [#(#raw_names),*];
                    // SAFETY: the raw argument array and runtime belong to
                    // this Excel ABI invocation.
                    #krate::__private::with_excel_call_scope(|__call_scope| {
                        let mut __return_context = unsafe {
                            #krate::__private::ReturnContext::for_call(
                                &crate::__XLFN_RUNTIME,
                                #udf_id,
                                &__raw_arguments,
                                __call_scope,
                            )
                        };
                        #krate::convert::ExcelReturn::invoke(
                            &mut __return_context,
                            || {
                                #context_setup
                                #(#conversions)*
                                ::core::result::Result::Ok(#invocation)
                            },
                        )
                    })
                },
            )
        }
    };
    let wrapper = if is_async {
        quote! {
            #gating
            #[doc = concat!("Excel async ABI wrapper for `", #excel_name, "`.")]
            #[doc = ""]
            #[doc = "# Safety"]
            #[doc = "Every argument pointer and the async handle must be a live XLOPER12 supplied by Excel for this call."]
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn #export_ident(
                #(#raw_names: *mut #krate::sys::XLOPER12,)*
                __async_handle: *mut #krate::sys::XLOPER12,
            ) {
                #boundary
            }
        }
    } else {
        quote! {
            #gating
            #[doc = concat!("Excel ABI wrapper for `", #excel_name, "`.")]
            #[doc = ""]
            #[doc = "# Safety"]
            #[doc = "Every argument pointer must be a live XLOPER12 supplied by Excel for this call."]
            #[unsafe(no_mangle)]
            pub unsafe extern "system" fn #export_ident(
                #(#raw_names: *mut #krate::sys::XLOPER12),*
            ) -> *mut #krate::sys::XLOPER12 {
                #boundary
            }
        }
    };

    Ok(quote! {
        #function

        #gating
        #[doc(hidden)]
        #[allow(non_upper_case_globals, reason = "Generated registration descriptor identifier")]
        static #descriptor_ident: #krate::__private::RegistrationDescriptor =
            #krate::__private::RegistrationDescriptor {
                export_name: stringify!(#export_ident),
                excel_name: #excel_name,
                signature: #krate::__private::RegistrationSignature {
                    result: if #is_async {
                        #krate::__private::ResultAbi::AsyncVoid
                    } else {
                        #krate::__private::ResultAbi::Xloper
                    },
                    arguments: &[#(#argument_abis),*],
                    flags: #krate::__private::RegistrationFlags {
                        thread_safe: #thread_safe,
                        macro_sheet: #macro_sheet,
                        volatile: #volatile,
                    },
                },
                category: #category,
                description: #description,
                help_topic: #help_topic,
                visibility: #visibility,
                arguments: &[
                    #(
                        #krate::__private::ArgumentDescriptor {
                            name: #argument_name_literals,
                            description: #argument_descriptions,
                        }
                    ),*
                ],
            };

        #gating
        #krate::__private::inventory::submit! {
            #descriptor_ident
        }

        #gating
        #[cfg(all(target_os = "windows", target_arch = "x86", target_env = "msvc"))]
        #[doc(hidden)]
        #[allow(non_upper_case_globals, reason = "Generated export directive identifier")]
        #[used]
        #[unsafe(link_section = ".drectve")]
        static #export_directive_ident: [u8; #x86_export_directive.len()] =
            *#x86_export_directive;

        #gating
        #[doc(hidden)]
        #[allow(non_upper_case_globals, reason = "Generated export symbol identifier")]
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static #export_manifest_entry_ident: [u8; #export_name_bytes.len()] =
            *#export_name_bytes;

        #wrapper
    })
}

fn expand_excel_addin(
    attributes: proc_macro2::TokenStream,
    item: ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let options = parse_addin_options(attributes)?;
    let krate = resolve_crate_path(options.krate.as_ref());
    let gating = gating_tokens(&item.attrs);
    if !item.generics.params.is_empty() || item.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "Add-in types cannot be generic",
        ));
    }
    let ident = &item.ident;
    let display_name = options.name.unwrap_or_else(|| ident.to_string());
    let id = options
        .id
        .unwrap_or_else(|| ident.to_string().to_ascii_lowercase());
    let category = options.category.unwrap_or_else(|| display_name.clone());
    validate_addin_metadata(&display_name, &id, &category, &item)?;
    Ok(quote! {
        #item

        #gating
        impl #krate::addin::AddinMetadata for #ident {
            const ID: &'static str = #id;
            const DISPLAY_NAME: &'static str = #display_name;
            const DEFAULT_CATEGORY: &'static str = #category;
        }

        #gating
        #[doc(hidden)]
        static __XLFN_RUNTIME: #krate::__private::Runtime<
            <#ident as #krate::addin::Addin>::State,
        > = #krate::__private::Runtime::new();

        #gating
        #[doc(hidden)]
        static __XLFN_ADDIN_ID: ::std::sync::OnceLock<
            ::core::result::Result<
                #krate::__private::AddinId,
                #krate::__private::InvalidAddinId,
            >
        > = ::std::sync::OnceLock::new();

        #gating
        #[cfg(all(target_os = "windows", target_arch = "x86", target_env = "msvc"))]
        #[used]
        #[unsafe(link_section = ".drectve")]
        static __XLFN_LIFECYCLE_EXPORTS: [u8; b" /EXPORT:xlAutoOpen=_xlAutoOpen@0 /EXPORT:xlAutoClose=_xlAutoClose@0 /EXPORT:xlAutoFree12=_xlAutoFree12@4 /EXPORT:xlAddInManagerInfo12=_xlAddInManagerInfo12@4 /EXPORT:DllGetClassObject=_DllGetClassObject@12 /EXPORT:DllCanUnloadNow=_DllCanUnloadNow@0".len()] =
            *b" /EXPORT:xlAutoOpen=_xlAutoOpen@0 /EXPORT:xlAutoClose=_xlAutoClose@0 /EXPORT:xlAutoFree12=_xlAutoFree12@4 /EXPORT:xlAddInManagerInfo12=_xlAddInManagerInfo12@4 /EXPORT:DllGetClassObject=_DllGetClassObject@12 /EXPORT:DllCanUnloadNow=_DllCanUnloadNow@0";

        #gating
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_FRAMEWORK_EXPORTS: [u8; b"xlAutoOpen\0xlAutoClose\0xlAutoFree12\0xlAddInManagerInfo12\0DllGetClassObject\0DllCanUnloadNow\0".len()] =
            *b"xlAutoOpen\0xlAutoClose\0xlAutoFree12\0xlAddInManagerInfo12\0DllGetClassObject\0DllCanUnloadNow\0";

        #gating
        #krate::__xlfn_async_exports!(&crate::__XLFN_RUNTIME);

        #gating
        #[unsafe(no_mangle)]
        pub extern "system" fn xlAutoOpen() -> i32 {
            let __addin_id = match crate::__XLFN_ADDIN_ID.get_or_init(|| {
                #krate::__private::AddinId::parse(
                    <#ident as #krate::addin::AddinMetadata>::ID,
                )
            }) {
                Ok(__addin_id) => __addin_id,
                Err(_) => return 0,
            };
            let mut __descriptors =
                #krate::__private::inventory::iter::<
                    #krate::__private::RegistrationDescriptor
                >
                .into_iter()
                .copied()
                .collect::<::std::vec::Vec<_>>();
            for __descriptor in &mut __descriptors {
                if __descriptor.category.is_empty() {
                    __descriptor.category =
                        <#ident as #krate::addin::AddinMetadata>::DEFAULT_CATEGORY;
                }
            }
            __descriptors.sort_unstable_by_key(|__descriptor| __descriptor.excel_name);
            #krate::__private::open_addin::<#ident>(
                &crate::__XLFN_RUNTIME,
                __addin_id,
                env!("CARGO_PKG_VERSION"),
                #krate::__private::BUILD_TARGET,
                &__descriptors,
            )
        }

        #gating
        #[unsafe(no_mangle)]
        pub extern "system" fn xlAutoClose() -> i32 {
            #krate::__private::close_addin::<#ident>(&crate::__XLFN_RUNTIME)
        }

        /// Releases one return pointer supplied back by Excel.
        ///
        /// # Safety
        /// The pointer must be the live value returned by this XLL and used once.
        #gating
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAutoFree12(
            __pointer: *mut #krate::sys::XLOPER12,
        ) {
            // SAFETY: Excel passes the live return pointer produced by this XLL.
            let __free_operation =
                unsafe { #krate::__private::free_return_boundary(__pointer) };
            ::core::mem::drop(__free_operation);
        }

        /// Supplies Add-in metadata to Excel's Add-in Manager.
        ///
        /// # Safety
        /// `action` must be a live XLOPER12 supplied by Excel.
        #gating
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAddInManagerInfo12(
            __action: *mut #krate::sys::XLOPER12,
        ) -> *mut #krate::sys::XLOPER12 {
            #krate::__private::ffi_boundary(
                &crate::__XLFN_RUNTIME,
                || {
                    #krate::__private::with_excel_call_scope(|__call_scope| {
                        // SAFETY: Excel supplies `__action` as a live XLOPER12
                        // for this ABI call.
                        let __action: f64 = unsafe {
                            #krate::__private::argument_from_raw(
                                __call_scope,
                                "action",
                                __action,
                            )?
                        };
                        if __action == 1.0 {
                            ::core::result::Result::Ok(
                                <#ident as #krate::addin::AddinMetadata>::DISPLAY_NAME.to_owned(),
                            )
                        } else {
                            ::core::result::Result::Err(#krate::error::XllError::input(
                                "action",
                                #krate::error::InputError::OutOfRange,
                            ))
                        }
                    })
                },
            )
        }

        #gating
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn DllGetClassObject(
            __class_id: *const ::core::ffi::c_void,
            __interface_id: *const ::core::ffi::c_void,
            __output: *mut *mut ::core::ffi::c_void,
        ) -> i32 {
            // SAFETY: Excel/COM supplies the three live ABI pointers for this
            // entry point, and the boundary validates their use.
            unsafe {
                #krate::__private::dll_get_class_object(
                    __class_id,
                    __interface_id,
                    __output,
                )
            }
        }

        #gating
        #[unsafe(no_mangle)]
        pub extern "system" fn DllCanUnloadNow() -> i32 {
            #krate::__private::dll_can_unload_now()
        }
    })
}

#[derive(Clone, Copy)]
enum ContextKind {
    ThreadSafe,
    MainThread,
    MacroSheet,
    Async,
}

fn parse_context_attribute(attribute: &syn::Attribute) -> syn::Result<ContextKind> {
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

fn parse_function_options(tokens: proc_macro2::TokenStream) -> syn::Result<FunctionOptions> {
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

fn doc_comment(attributes: &[syn::Attribute]) -> String {
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

fn parse_addin_options(tokens: proc_macro2::TokenStream) -> syn::Result<AddinOptions> {
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

fn validate_addin_metadata(
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

fn parse_meta(tokens: proc_macro2::TokenStream) -> syn::Result<Punctuated<Meta, Token![,]>> {
    Punctuated::<Meta, Token![,]>::parse_terminated.parse2(tokens)
}

fn string_value(expression: &Expr, name: &str) -> syn::Result<String> {
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

fn validate_export_id(id: &str, span: &impl quote::ToTokens) -> syn::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn function(source: proc_macro2::TokenStream) -> ItemFn {
        syn::parse2(source).unwrap()
    }

    #[test]
    fn plain_return_and_volatile_option_expand() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ONE", thread_safe, volatile),
            function(quote!(
                fn one(value: f64) -> f64 {
                    value
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("volatile : true"));
        assert!(expanded.contains("ExcelReturn :: invoke"));
        assert!(expanded.contains("argument_from_raw_with_context"));
        assert!(expanded.contains("ReturnContext :: for_call"));
        assert!(expanded.contains("assert_thread_safe_return"));
        assert!(expanded.contains("assert_volatile_return"));
    }

    #[test]
    fn registration_and_argument_policies_expand_into_the_existing_descriptor() {
        let expanded = expand_excel_function(
            quote!(
                name = "TEST.METADATA",
                help_topic = "https://example.test/help",
                hidden
            ),
            function(quote!(
                fn metadata(
                    #[excel_arg(
                        name = "Factor",
                        description = "Parameter size",
                        default = 1_000_000.0,
                        blank = "default",
                        missing = "default"
                    )]
                    factor: f64,
                ) -> f64 {
                    factor
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("help_topic : \"https://example.test/help\""));
        assert!(expanded.contains("FunctionVisibility :: Hidden"));
        assert!(expanded.contains("CellPresence :: Blank"));
        assert!(expanded.contains("CellPresence :: Missing"));
        assert!(expanded.contains("name : \"Factor\""));
    }

    #[test]
    fn default_without_blank_or_missing_policy_is_rejected() {
        let error = expand_excel_function(
            quote!(),
            function(quote!(
                fn func(#[excel_arg(default = 1.0)] arg: f64) -> f64 {
                    arg
                }
            )),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(
                "`default = ...` requires `blank = \"default\"` or `missing = \"default\"`"
            ),
            "got error: {error}"
        );

        let error = expand_excel_function(
            quote!(),
            function(quote!(
                fn func(
                    #[excel_arg(default = 1.0, blank = "error", missing = "error")] arg: f64,
                ) -> f64 {
                    arg
                }
            )),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(
                "`default = ...` requires `blank = \"default\"` or `missing = \"default\"`"
            ),
            "got error: {error}"
        );
    }

    #[test]
    fn removed_stringly_typed_function_options_are_rejected() {
        for attributes in [
            quote!(visibility = "hidden"),
            quote!(overwrite = "deny"),
            quote!(handle),
        ] {
            let error = expand_excel_function(
                attributes,
                function(quote!(
                    fn replacement() -> f64 {
                        0.0
                    }
                )),
            )
            .unwrap_err();
            assert!(error.to_string().contains("expected `name`"));
        }
    }

    #[test]
    fn removed_handle_argument_role_is_rejected() {
        let error = expand_excel_function(
            quote!(),
            function(quote!(
                fn consume(#[excel_arg(handle)] value: Handle<Dataset>) -> f64 {
                    value.size
                }
            )),
        )
        .unwrap_err();
        assert!(error.to_string().contains("expected `name`"));
    }

    #[test]
    fn excel_argument_name_delimiters_are_rejected_during_expansion() {
        for name in ["", "start,end", "nul\0name", "line\rbreak", "line\nbreak"] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(
                    fn invalid_name(#[excel_arg(name = #name)] value: f64) -> f64 {
                        value
                    }
                )),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Excel argument names must be non-empty counted strings")
            );
        }
    }

    #[test]
    fn excel_enum_generates_static_bidirectional_conversion() {
        let input: DeriveInput = syn::parse2(quote! {
            #[excel_enum(ascii_case_insensitive)]
            enum Mode {
                Ascending,
                #[excel_value(name = "P")]
                Descending,
            }
        })
        .unwrap();
        let expanded = expand_excel_enum(input).unwrap().to_string();
        assert!(expanded.contains("eq_ignore_ascii_case"));
        assert!(expanded.contains("FromExcel"));
        assert!(expanded.contains("IntoExcelValue"));
        assert!(expanded.contains("ExcelReturn"));
        assert!(expanded.contains("MainThreadReturn"));
        assert!(expanded.contains("ThreadSafeReturn"));
        assert!(expanded.contains("MacroSheetReturn"));
        assert!(expanded.contains("AsyncReturn"));
        assert!(expanded.contains("VolatileReturn"));
        assert!(!expanded.contains("HandleKey"));
    }

    #[test]
    fn excel_handle_object_generates_the_complete_return_contract() {
        let input: DeriveInput = syn::parse2(quote! {
            struct Dataset<T> {
                value: T,
            }
        })
        .unwrap();
        let expanded = expand_excel_handle_object(input).unwrap().to_string();
        assert!(expanded.contains("ExcelHandleObject for Dataset"));
        assert!(expanded.contains("ExcelReturn for Dataset"));
        assert!(expanded.contains("publish_new_handle"));
        assert!(expanded.contains("MainThreadReturn for Dataset"));
        assert!(!expanded.contains("VolatileReturn"));
        assert!(!expanded.contains("ExcelHandleType"));
        assert!(!expanded.contains("NAME"));
    }

    #[test]
    fn context_and_return_type_drive_registration_and_boundary() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.VALUE"),
            function(quote!(
                /// Function Wizard help.
                fn value(
                    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, State>,
                ) -> i32 {
                    1
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("thread_safe : true"));
        assert!(expanded.contains("Function Wizard help."));
        assert!(expanded.contains("ExcelReturn :: invoke"));
        assert!(expanded.contains("let __context : ThreadSafeContext"));
        assert!(!expanded.contains("& __generated_context"));
    }

    #[test]
    fn context_role_comes_only_from_the_attribute() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ALIAS"),
            function(quote!(
                fn value(#[excel_context(main_thread)] context: MainContext<'_>) -> i32 {
                    1
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("let __context : MainContext"));
        assert!(expanded.contains("MainThreadContext :: new"));
    }

    #[test]
    fn async_function_uses_native_async_boundary_and_owned_context() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ASYNC"),
            function(quote!(
                async fn value(
                    #[excel_context(asynchronous)] context: AsyncContext<State>,
                    input: f64,
                ) -> XllResult<f64> {
                    Ok(input)
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("async_udf_boundary_named"));
        assert!(!expanded.contains("ffi_boundary_void"));
        assert!(expanded.contains("ResultAbi :: AsyncVoid"));
        assert!(expanded.contains("__async_handle"));
        assert!(expanded.contains("thread_safe : true"));
        assert!(expanded.contains("__xlfn_async_only !"));
        assert!(!expanded.contains("AsyncFeature"));
        assert!(expanded.contains("assert_async_return"));
        assert!(expanded.contains("@8"));
    }

    #[test]
    fn async_function_rejects_a_non_async_context_role() {
        let context_error = expand_excel_function(
            quote!(name = "TEST.ASYNC"),
            function(quote!(
                async fn value(
                    #[excel_context(thread_safe)] context: &ThreadSafeContext<'_, State>,
                ) -> f64 {
                    1.0
                }
            )),
        )
        .unwrap_err();
        assert!(context_error.to_string().contains("asynchronous"));
    }

    #[test]
    fn raw_reference_requires_macro_sheet_execution() {
        let error = expand_excel_function(
            quote!(name = "TEST.REF"),
            function(quote!(
                fn bad(#[excel_arg(reference)] value: ExcelReference<'_>) -> XllResult<i32> {
                    Ok(1)
                }
            )),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires `macro_sheet` or MacroSheetContext")
        );
    }

    #[test]
    fn macro_sheet_flag_supports_reference_arguments_without_context_injection() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.REF", macro_sheet),
            function(quote!(
                fn reference(#[excel_arg(reference)] value: ExcelReference<'_>) -> i32 {
                    1
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("macro_sheet : true"));
        assert!(expanded.contains("assert_macro_sheet_return"));
        assert!(expanded.contains("reference_from_raw"));
    }

    #[test]
    fn macro_sheet_cannot_be_thread_safe() {
        let error = expand_excel_function(
            quote!(name = "TEST.REF", thread_safe),
            function(quote!(
                fn bad(
                    #[excel_context(macro_sheet)] context: &MacroSheetContext<'_, State>,
                ) -> XllResult<i32> {
                    Ok(1)
                }
            )),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot be marked `thread_safe`"));

        let error = expand_excel_function(
            quote!(name = "TEST.REF", macro_sheet, thread_safe),
            function(quote!(
                fn bad() -> f64 {
                    0.0
                }
            )),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cannot be marked `thread_safe`"));
    }

    #[test]
    fn gating_tokens_filter_non_gating_cfg_attr_payloads() {
        let item = function(quote! {
            #[cfg_attr(feature = "gated", cfg(any()), allow(dead_code))]
            fn gated() -> f64 { 1.0 }
        });
        let gating = gating_tokens(&item.attrs).to_string();
        assert!(gating.contains("cfg_attr"));
        assert!(gating.contains("cfg (any ())"));
        assert!(!gating.contains("allow"));
    }

    #[test]
    fn excel_function_propagates_cfg_to_generated_items() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.GATED"),
            function(quote! {
                #[cfg(feature = "gated")]
                fn gated() -> f64 { 1.0 }
            }),
        )
        .unwrap()
        .to_string();
        assert!(expanded.matches(r#"cfg (feature = "gated")"#).count() >= 6);
    }

    #[test]
    fn excel_addin_propagates_cfg_to_lifecycle_exports() {
        let item: ItemStruct = syn::parse2(quote! {
            #[cfg(feature = "gated")]
            struct GatedAddin;
        })
        .unwrap();
        let expanded = expand_excel_addin(quote!(), item).unwrap().to_string();
        assert!(expanded.matches(r#"cfg (feature = "gated")"#).count() >= 10);
    }

    #[test]
    fn object_return_uses_the_common_lazy_return_pipeline() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.DATASET"),
            function(quote! {
                fn dataset(
                    #[excel_arg(default = 0.0, missing = "default")] rate: f64,
                ) -> Dataset {
                    todo!()
                }
            }),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("ExcelReturn :: invoke"));
        assert!(expanded.contains("assert_main_thread_return"));
        assert!(expanded.contains("ReturnContext :: for_call"));
        assert!(expanded.contains("& __raw_arguments"));
        assert!(
            expanded.find("ExcelReturn :: invoke").unwrap()
                < expanded.find("argument_from_raw_with_context").unwrap(),
            "formula identity must be established before handle factory evaluation"
        );
        assert!(!expanded.contains("ExcelHandleReturn"));
        assert!(!expanded.contains("HandleKey"));
    }

    #[test]
    fn return_mode_bounds_follow_execution_mode_without_type_classification() {
        let thread_safe = expand_excel_function(
            quote!(name = "TEST.THREAD", thread_safe),
            function(quote! {
                fn value() -> ReturnAlias { todo!() }
            }),
        )
        .unwrap()
        .to_string();
        assert!(thread_safe.contains("assert_thread_safe_return"));
        assert!(thread_safe.contains("ReturnAlias"));

        let asynchronous = expand_excel_function(
            quote!(name = "TEST.ASYNC"),
            function(quote! {
                async fn value() -> ReturnAlias { todo!() }
            }),
        )
        .unwrap()
        .to_string();
        assert!(asynchronous.contains("assert_async_return"));

        let macro_sheet = expand_excel_function(
            quote!(name = "TEST.MACRO", macro_sheet),
            function(quote! {
                fn value() -> ReturnAlias { todo!() }
            }),
        )
        .unwrap()
        .to_string();
        assert!(macro_sheet.contains("assert_macro_sheet_return"));
    }

    #[test]
    fn addin_attribute_owns_runtime_and_lifecycle_exports() {
        let item: ItemStruct = syn::parse2(quote! {
            pub struct TestAddin;
        })
        .unwrap();
        let expanded = expand_excel_addin(
            quote!(name = "Test Add-in", id = "test-addin", category = "Test"),
            item,
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("static __XLFN_RUNTIME"));
        assert!(expanded.contains("fn xlAutoOpen"));
        assert!(expanded.contains("fn xlAutoClose"));
        assert!(expanded.contains("fn xlAutoFree12"));
        assert!(expanded.contains("let __free_operation"));
        assert!(expanded.contains("free_return_boundary"));
        assert!(expanded.contains("fn xlAddInManagerInfo12"));
        assert!(expanded.contains("ffi_boundary"));
        assert!(expanded.contains("fn DllGetClassObject"));
        assert!(expanded.contains("fn DllCanUnloadNow"));
        assert!(expanded.contains("__XLFN_FRAMEWORK_EXPORTS"));
        assert!(expanded.contains("__xlfn_async_exports"));
        assert!(!expanded.contains("fn __xlfn_calculation_canceled"));
    }

    #[test]
    fn blank_and_missing_error_policies_expand_error_branches() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.STRICT"),
            function(quote!(
                fn strict(
                    #[excel_arg(blank = "error", missing = "error")] value: Option<f64>,
                ) -> f64 {
                    value.unwrap_or(0.0)
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("blank cell is not allowed"));
        assert!(expanded.contains("missing argument is not allowed"));
    }

    #[test]
    fn explicit_crate_override_replaces_default_crate_path() {
        let func_expanded = expand_excel_function(
            quote!(name = "TEST.CUSTOM", crate = "my_custom_xlfn"),
            function(quote!(
                fn custom(value: f64) -> f64 {
                    value
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(func_expanded.contains("my_custom_xlfn :: __private"));
        assert!(func_expanded.contains("my_custom_xlfn :: convert"));

        let addin_item: ItemStruct = syn::parse2(quote!(
            struct MyAddin;
        ))
        .unwrap();
        let addin_expanded = expand_excel_addin(quote!(crate = my_custom_xlfn), addin_item)
            .unwrap()
            .to_string();
        assert!(addin_expanded.contains("my_custom_xlfn :: addin :: AddinMetadata"));
        assert!(addin_expanded.contains("my_custom_xlfn :: __private :: Runtime"));
    }
}
