//! Syntax inspected by `excel_function`, with an opaque Rust function body.

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::parse::{Parse, ParseStream};
use syn::{AttrStyle, Attribute, Signature, Visibility, braced, token};

/// Keep the original body tokens and spans. The macro only changes parameter
/// attributes; parsing every body expression duplicates the compiler's work.
pub(crate) struct UdfFunction {
    pub(crate) attrs: Vec<Attribute>,
    vis: Visibility,
    pub(crate) sig: Signature,
    brace: token::Brace,
    body: TokenStream,
}

impl Parse for UdfFunction {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut attrs = input.call(Attribute::parse_outer)?;
        let vis = input.parse()?;
        let sig = input.parse()?;
        let content;
        let brace = braced!(content in input);
        attrs.extend(content.call(Attribute::parse_inner)?);
        let body = content.parse()?;
        Ok(Self {
            attrs,
            vis,
            sig,
            brace,
            body,
        })
    }
}

impl ToTokens for UdfFunction {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        for attribute in &self.attrs {
            if matches!(attribute.style, AttrStyle::Outer) {
                attribute.to_tokens(tokens);
            }
        }
        self.vis.to_tokens(tokens);
        self.sig.to_tokens(tokens);
        self.brace.surround(tokens, |tokens| {
            for attribute in &self.attrs {
                if matches!(attribute.style, AttrStyle::Inner(_)) {
                    attribute.to_tokens(tokens);
                }
            }
            self.body.to_tokens(tokens);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn body_tokens_include_inner_attributes_and_nested_rust_syntax() {
        let source = quote! {
            #[doc = "retained"]
            pub(crate) async fn example(value: i32) -> i32 {
                #![allow(unused_braces)]
                macro_rules! identity { ($value:expr) => { $value }; }
                let nested = async { identity!({ value }) };
                nested.await
            }
        };
        let function: UdfFunction = syn::parse2(source.clone()).unwrap();
        // Inner attributes remain available to metadata and gating analysis,
        // just as they are on syn::ItemFn.
        assert_eq!(function.attrs.len(), 2);
        assert_eq!(function.into_token_stream().to_string(), source.to_string());
    }

    #[test]
    fn rejects_missing_bodies_and_trailing_items() {
        assert!(
            syn::parse2::<UdfFunction>(quote!(
                fn example();
            ))
            .is_err()
        );
        assert!(
            syn::parse2::<UdfFunction>(quote!(
                fn example() {}
                fn extra() {}
            ))
            .is_err()
        );
    }
}
