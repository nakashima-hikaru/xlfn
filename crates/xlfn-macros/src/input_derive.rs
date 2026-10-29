//! Eager custom-input conveniences; all identity encoding stays trait based.

use crate::support::resolve_crate_path;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

pub(super) fn identity(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = resolve_crate_path(None);
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
                quote!(#krate::value::ExcelInputIdentity::encode_input_identity(&self.#member, encoder);)
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
                        encoder.u64(#tag);
                        #(#krate::value::ExcelInputIdentity::encode_input_identity(#names, encoder);)*
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
    for ty in field_types {
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
            fn encode_input_identity(&self, encoder: &mut #krate::value::InputIdentityEncoder) {
                #body
            }
        }
    })
}

pub(super) fn prepare(input: DeriveInput) -> syn::Result<TokenStream> {
    let krate = resolve_crate_path(None);
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
                value: #krate::value::XlValueRef<#lifetime>,
                argument: &'static str,
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
