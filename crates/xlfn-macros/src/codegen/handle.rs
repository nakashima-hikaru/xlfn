use quote::quote;
use syn::DeriveInput;

use crate::options::{parse_expr_path, resolve_crate_path};

pub(crate) fn expand_excel_handle_object(
    input: DeriveInput,
) -> syn::Result<proc_macro2::TokenStream> {
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
                let expr: syn::Expr = meta.value()?.parse()?;
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

        impl #impl_generics #krate::__private::ExcelReturn
            for #ident #type_generics #where_clause
        {
            type InputMode = #krate::__private::FormulaInputMode;

            fn invoke(
                __context: &mut #krate::__private::ReturnContext<'_, '_>,
                __operation: impl ::core::ops::FnOnce()
                    -> #krate::error::XllResult<Self>,
            ) -> #krate::error::XllResult<#krate::__private::ExcelOutput> {
                #krate::__private::publish_new_handle(__context, __operation)
            }

            fn into_excel(
                self,
                __context: &mut #krate::__private::ReturnContext<'_, '_>,
            ) -> #krate::error::XllResult<#krate::__private::ExcelOutput> {
                #krate::__private::publish_new_handle(
                    __context,
                    || ::core::result::Result::Ok(self),
                )
            }
        }

        impl #impl_generics #krate::__private::MainThreadReturn
            for #ident #type_generics #where_clause
        {}
    })
}
