use std::collections::BTreeSet;

use quote::quote;
use syn::{Data, DeriveInput, Expr, Fields};

use crate::options::parse_expr_path;
use crate::support::resolve_crate_path;

pub(crate) fn expand_excel_enum(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
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
                if #krate::__private::v1::utf16_eq_ignore_ascii_case(
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
    let identities = variants
        .iter()
        .enumerate()
        .map(|(index, (variant, _))| quote!(Self::#variant => #index as u32))
        .collect::<Vec<_>>();
    Ok(quote! {
        impl #from_excel_impl_generics #krate::value::FromExcel<'__xlfn_call>
            for #ident #type_generics #from_excel_where_clause
        {
            fn from_excel(
                __value: #krate::value::XlValueRef<'__xlfn_call>,
                __argument: &'static str,
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

        impl #base_impl_generics #krate::value::IntoExcel
            for #ident #type_generics #base_where_clause
        {
            fn into_excel(self) -> #krate::error::XllResult<#krate::value::ExcelCellOutput> {
                let __text = match self {
                    #(#outputs,)*
                };
                ::core::result::Result::Ok(
                    #krate::value::ExcelCellOutput::String(__text.to_owned())
                )
            }
        }

        impl #base_impl_generics #krate::value::ExcelInputIdentity
            for #ident #type_generics #base_where_clause
        {
            fn encode_input_identity(
                &self,
                __encoder: &mut #krate::value::InputIdentityEncoder,
            ) {
                let __variant = match self {
                    #(#identities,)*
                };
                __encoder.u32(__variant);
            }
        }
    })
}
