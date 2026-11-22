//! Eager custom-input conveniences; all identity encoding stays trait based.

use crate::support::resolve_crate_path;
use proc_macro2::{Ident, TokenStream, TokenTree};
use quote::{ToTokens, quote};
use syn::ext::IdentExt;
use syn::{Data, DeriveInput, Fields};

fn crate_path(attributes: &[syn::Attribute]) -> syn::Result<syn::Path> {
    let mut explicit = None;
    for attribute in attributes {
        if !attribute.path().is_ident("excel_input") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("crate") {
                return Err(meta.error("expected `crate = \"...\"`"));
            }
            if explicit.is_some() {
                return Err(meta.error("duplicate `crate` option"));
            }
            let expression: syn::Expr = meta.value()?.parse()?;
            explicit = Some(crate::options::parse_expr_path(&expression)?);
            Ok(())
        })?;
    }
    Ok(resolve_crate_path(explicit.as_ref()))
}

fn mentions_generic(tokens: TokenStream, parameters: &[Ident]) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => parameters.contains(&ident.unraw()),
        TokenTree::Group(group) => mentions_generic(group.stream(), parameters),
        _ => false,
    })
}

pub(super) fn identity(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = crate_path(&input.attrs)?;
    let name = input.ident;
    let mut generics = input.generics;
    let mut field_types = Vec::new();
    let body = match input.data {
        Data::Struct(data) => {
            let fields = data.fields.iter().enumerate().map(|(index, field)| {
                field_types.push(field.ty.clone());
                let member = field.ident.clone().map_or_else(
                    || syn::Member::Unnamed(syn::Index::from(index)),
                    syn::Member::Named,
                );
                quote!(#krate::value::ExcelInputIdentity::encode_input_identity(&self.#member, _encoder);)
            }).collect::<Vec<_>>();
            quote!(#(#fields)*)
        }
        Data::Enum(data) => {
            let arms = data.variants.iter().enumerate().map(|(index, variant)| {
                let variant_name = &variant.ident;
                let mut names = Vec::new();
                for (index, field) in variant.fields.iter().enumerate() {
                    field_types.push(field.ty.clone());
                    names.push(syn::Ident::new(&format!("__xlfn_field_{index}"), name.span()));
                }
                let pattern = match &variant.fields {
                    Fields::Named(fields) => {
                        let members = fields.named.iter().map(|field| &field.ident);
                        quote!({#(#members: #names),*})
                    }
                    Fields::Unnamed(_) => quote!((#(#names),*)),
                    Fields::Unit => quote!(),
                };
                let tag = index as u64;
                quote! {
                    Self::#variant_name #pattern => {
                        _encoder.u64(#tag);
                        #(#krate::value::ExcelInputIdentity::encode_input_identity(#names, _encoder);)*
                    }
                }
            }).collect::<Vec<_>>();
            quote!(match self { #(#arms),* })
        }
        Data::Union(data) => {
            return Err(syn::Error::new_spanned(
                data.union_token,
                "ExcelInputIdentity cannot be derived for a union",
            ));
        }
    };
    let parameters: Vec<_> = generics
        .params
        .iter()
        .map(|parameter| match parameter {
            syn::GenericParam::Type(parameter) => parameter.ident.unraw(),
            syn::GenericParam::Lifetime(parameter) => parameter.lifetime.ident.unraw(),
            syn::GenericParam::Const(parameter) => parameter.ident.unraw(),
        })
        .collect();
    for ty in field_types {
        // The body already checks concrete fields without imposing a public
        // bound. An explicit bound on a recursive concrete field also makes
        // proving the derived impl depend on itself.
        if !mentions_generic(ty.to_token_stream(), &parameters) {
            continue;
        }
        generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote!(
                #ty: #krate::value::ExcelInputIdentity
            ));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::value::ExcelInputIdentity for #name #ty_generics #where_clause {
            fn encode_input_identity(&self, _encoder: &mut #krate::value::InputIdentityEncoder) {
                #body
            }
        }
    })
}

pub(super) fn prepare(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = crate_path(&input.attrs)?;
    let name = input.ident;
    let mut generics = input.generics.clone();
    let mut lifetime_name = "__xlfn_call".to_owned();
    while generics
        .lifetimes()
        .any(|parameter| parameter.lifetime.ident == lifetime_name)
    {
        lifetime_name.push('_');
    }
    let lifetime = syn::Lifetime::new(&format!("'{lifetime_name}"), name.span());
    generics.params.insert(0, syn::parse_quote!(#lifetime));
    let (_, ty_generics, _) = input.generics.split_for_impl();
    generics.make_where_clause().predicates.push(syn::parse_quote!(
        #name #ty_generics: #krate::value::FromExcel<#lifetime> + #krate::value::ExcelInputIdentity
    ));
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::value::PrepareExcel<#lifetime> for #name #ty_generics #where_clause {
            type Prepared = Self;
            fn prepare(
                value: #krate::value::raw::XlValueRef<#lifetime>,
                argument: &'static ::core::primitive::str,
                identity: &mut #krate::value::InputIdentityEncoder,
            ) -> #krate::XllResult<Self> {
                <Self as #krate::value::FromExcel<#lifetime>>::from_excel_with_identity(value, argument, identity)
            }
            fn materialize(prepared: Self) -> #krate::XllResult<Self> {
                ::core::result::Result::Ok(prepared)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_input_derives_accept_an_explicit_facade_crate() {
        for expand in [identity, prepare] {
            let input = syn::parse_quote!(
                #[excel_input(crate = "facade::api")]
                struct Input;
            );
            let tokens = expand(input).unwrap().to_string();
            assert!(tokens.contains("facade :: api :: value"));
        }
    }

    #[test]
    fn input_derive_options_reject_duplicates_and_unknown_fields() {
        for expand in [identity, prepare] {
            let duplicate = syn::parse_quote!(
                #[excel_input(crate = "first")]
                #[excel_input(crate = "second")]
                struct Input;
            );
            assert!(
                expand(duplicate)
                    .unwrap_err()
                    .to_string()
                    .contains("duplicate")
            );
            let unknown = syn::parse_quote!(
                #[excel_input(unknown = "value")]
                struct Input;
            );
            assert!(
                expand(unknown)
                    .unwrap_err()
                    .to_string()
                    .contains("expected")
            );
        }
    }
}
