//! Attribute and derive macros for the public `xlfn` API.
//!
//! The macros generate the Excel ABI entry points, registration metadata, and
//! add-in lifecycle exports consumed by `xlfn`. They intentionally keep raw
//! pointer handling at the generated FFI boundary; user functions receive the
//! safe values and contexts exposed by `xlfn`.
//!
//! # Stability and Supported API Policy
//!
//! `xlfn-macros` is an implementation crate for the `xlfn` framework. The only
//! supported public API for add-in authors is the `xlfn` facade crate.
//! Direct use of `xlfn-macros` does not carry semantic versioning stability
//! guarantees.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemFn, ItemStruct, parse_macro_input};
mod codegen;
mod model;
mod options;
mod support;
mod validation;

use options::parse_addin_options;
use support::{extract_gating_attributes, resolve_crate_path};
use validation::validate_addin_metadata;

/// Attributes a function as an Excel UDF.
///
/// Excel-visible arguments and return values are selected by their conversion
/// trait implementations. An `async fn` selects asynchronous mode directly;
/// there is no `#[excel_function(async)]` mode flag. Injected contexts must be
/// the first parameter and carry an explicit `#[excel_context(...)]` role.
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
    codegen::expand_excel_enum(input)
}

fn expand_excel_handle_object(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    codegen::expand_excel_handle_object(input)
}

fn expand_excel_function(
    attributes: proc_macro2::TokenStream,
    function: ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let parsed = model::parse_udf(attributes, function)?;
    let spec = model::analyze(parsed)?;
    let plan = model::lower(spec);
    Ok(codegen::emit_excel_function(&plan))
}

fn expand_excel_addin(
    attributes: proc_macro2::TokenStream,
    item: ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let options = parse_addin_options(attributes)?;
    let krate = resolve_crate_path(options.krate.as_ref());
    let gating = extract_gating_attributes(&item.attrs);
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

        #(#gating)*
        #[doc(hidden)]
        static __XLFN_RUNTIME: #krate::__private::v1::MacroRuntime<
            #ident,
        > = #krate::__private::v1::MacroRuntime::new();

        #(#gating)*
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_FRAMEWORK_EXPORTS: [u8; b"xlAutoOpen\0xlAutoClose\0xlAutoRemove\0xlAutoFree12\0xlAddInManagerInfo12\0".len()] =
            *b"xlAutoOpen\0xlAutoClose\0xlAutoRemove\0xlAutoFree12\0xlAddInManagerInfo12\0";

        #(#gating)*
        #krate::__private::v1::__xlfn_async_exports!(&crate::__XLFN_RUNTIME);

        #(#gating)*
        #krate::__private::v1::__xlfn_rtd_exports!(&crate::__XLFN_RUNTIME);

        #(#gating)*
        #[unsafe(no_mangle)]
        pub extern "system" fn xlAutoOpen() -> i32 {
            #krate::__private::v1::open_generated_addin::<#ident>(
                &crate::__XLFN_RUNTIME,
                #id,
                #display_name,
                #category,
                env!("CARGO_PKG_VERSION"),
                #krate::__private::v1::BUILD_TARGET,
                xlAutoOpen as *const (),
            )
        }

        #(#gating)*
        #[unsafe(no_mangle)]
        pub extern "system" fn xlAutoClose() -> i32 {
            #krate::__private::v1::auto_close_generated_addin::<#ident>(&crate::__XLFN_RUNTIME)
        }

        #(#gating)*
        #[unsafe(no_mangle)]
        pub extern "system" fn xlAutoRemove() -> i32 {
            #krate::__private::v1::auto_remove_generated_addin::<#ident>(&crate::__XLFN_RUNTIME)
        }

        /// Releases one return pointer supplied back by Excel.
        ///
        /// # Safety
        /// The pointer must be the live value returned by this XLL and used once.
        #(#gating)*
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAutoFree12(
            __pointer: *mut #krate::__private::v1::XLOPER12,
        ) {
            // SAFETY: Excel passes the live return pointer produced by this XLL.
            unsafe { #krate::__private::v1::free_generated_return(__pointer) };
        }

        /// Supplies Add-in metadata to Excel's Add-in Manager.
        ///
        /// # Safety
        /// `action` must be a live XLOPER12 supplied by Excel.
        #(#gating)*
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAddInManagerInfo12(
            __action: *mut #krate::__private::v1::XLOPER12,
        ) -> *mut #krate::__private::v1::XLOPER12 {
            // SAFETY: Excel supplies `__action` as a live XLOPER12 for this ABI call.
            unsafe {
                #krate::__private::v1::addin_manager_info(
                    &crate::__XLFN_RUNTIME,
                    #display_name,
                    __action,
                )
            }
        }

    })
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
        assert!(expanded.contains("FunctionRegistration :: new"));
        assert!(expanded.contains("sync_udf"));
        assert!(expanded.contains("convert_argument"));
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
        assert!(expanded.contains("https://example.test/help"));
        assert!(expanded.contains("FunctionRegistration :: new"));
        assert!(expanded.contains("CellPresence :: Blank"));
        assert!(expanded.contains("CellPresence :: Missing"));
        assert!(expanded.contains("\"Factor\""));
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
                fn consume(#[excel_arg(handle)] value: Handle<'_, Dataset>) -> f64 {
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
        assert!(!expanded.contains("ExcelParameter"));
        assert!(expanded.contains("ExcelInputIdentity"));
        assert!(expanded.contains("encode_input_identity"));
        assert!(expanded.contains("__encoder . u32"));
        assert!(!expanded.contains("__encoder . domain"));
        assert!(expanded.contains("ExcelCellOutput"));
        assert!(expanded.contains("IntoExcel"));
        assert!(!expanded.contains("ExcelReturn"));
        assert!(!expanded.contains("MainThreadReturn"));
        assert!(!expanded.contains("ThreadSafeReturn"));
        assert!(!expanded.contains("MacroSheetReturn"));
        assert!(!expanded.contains("AsyncReturn"));
        assert!(!expanded.contains("VolatileReturn"));
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
        assert!(expanded.contains("FunctionRegistration :: new"));
        assert!(expanded.contains("Function Wizard help."));
        assert!(expanded.contains("sync_udf"));
        assert!(expanded.contains("thread_safe_context"));
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
        assert!(expanded.contains("main_thread_context"));
    }

    #[test]
    fn execution_mode_flags_cannot_repeat_context_roles() {
        let error = expand_excel_function(
            quote!(name = "TEST.DUPLICATE.THREAD", thread_safe),
            function(quote!(
                fn value(
                    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, State>,
                ) -> i32 {
                    let _ = context;
                    1
                }
            )),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must not repeat the `thread_safe`")
        );

        let error = expand_excel_function(
            quote!(name = "TEST.DUPLICATE.MACRO", macro_sheet),
            function(quote!(
                fn value(
                    #[excel_context(macro_sheet)] context: MacroSheetContext<'_, State>,
                ) -> i32 {
                    let _ = context;
                    1
                }
            )),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must not repeat the `macro_sheet`")
        );
    }

    #[test]
    fn async_function_uses_native_async_boundary_and_borrowed_context() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ASYNC"),
            function(quote!(
                async fn value(
                    #[excel_context(asynchronous)] context: AsyncContext<'_, State>,
                    input: f64,
                ) -> XllResult<f64> {
                    Ok(input)
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("async_udf"));
        assert!(expanded.contains("async_context"));
        assert!(!expanded.contains("ffi_boundary_void"));
        assert!(expanded.contains("__async_handle"));
        assert!(expanded.contains("FunctionRegistration :: new"));
        assert!(expanded.contains("__xlfn_async_only !"));
        assert!(!expanded.contains("AsyncFeature"));
        assert!(expanded.contains("assert_async_return"));
    }

    #[test]
    fn async_function_selects_async_mode_without_a_mode_attribute() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ASYNC.NO_CONTEXT"),
            function(quote!(
                async fn value(input: f64) -> XllResult<f64> {
                    Ok(input)
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("async_udf"));
        assert!(expanded.contains("assert_async_return"));
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
        assert!(expanded.contains("FunctionRegistration :: new"));
        assert!(expanded.contains("assert_macro_sheet_return"));
        assert!(expanded.contains("convert_reference"));
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
    fn gating_attributes_filter_non_gating_cfg_attr_payloads() {
        let item = function(quote! {
            #[cfg_attr(feature = "gated", cfg(any()), allow(dead_code))]
            fn gated() -> f64 { 1.0 }
        });
        let gating_attributes = extract_gating_attributes(&item.attrs);
        let gating = quote!(#(#gating_attributes)*).to_string();
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
        assert!(expanded.matches(r#"cfg (feature = "gated")"#).count() >= 5);
    }

    #[test]
    fn excel_addin_propagates_cfg_to_lifecycle_exports() {
        let item: ItemStruct = syn::parse2(quote! {
            #[cfg(feature = "gated")]
            struct GatedAddin;
        })
        .unwrap();
        let expanded = expand_excel_addin(quote!(), item).unwrap().to_string();
        assert!(expanded.matches(r#"cfg (feature = "gated")"#).count() >= 8);
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
        assert!(expanded.contains("sync_udf"));
        assert!(expanded.contains("assert_main_thread_return"));
        assert!(expanded.contains("convert_argument"));
        assert!(!expanded.contains("ExcelHandleReturn"));
        assert!(!expanded.contains("HandleKey"));
    }

    #[test]
    fn generated_wrapper_applies_default_presence_policies() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.DEFAULT.IDENTITY"),
            function(quote! {
                fn defaulted(
                    #[excel_arg(default = 1.0, blank = "default", missing = "default")]
                    value: f64,
                ) -> f64 {
                    value
                }
            }),
        )
        .unwrap()
        .to_string();

        let blank = expanded
            .find("CellPresence :: Blank =>")
            .expect("blank default branch must be generated");
        let missing = expanded
            .find("CellPresence :: Missing =>")
            .expect("missing default branch must be generated");
        assert!(blank < missing);
        assert!(expanded.contains("sync_udf"));
        assert!(expanded.contains("default_argument"));
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
        assert!(expanded.contains("open_generated_addin"));
        assert!(expanded.contains("fn xlAutoClose"));
        assert!(expanded.contains("auto_close_generated_addin"));
        assert!(expanded.contains("fn xlAutoRemove"));
        assert!(expanded.contains("auto_remove_generated_addin"));
        assert!(expanded.contains("fn xlAutoFree12"));
        assert!(expanded.contains("free_generated_return"));
        assert!(expanded.contains("fn xlAddInManagerInfo12"));
        assert!(expanded.contains("addin_manager_info"));
        assert!(expanded.contains("__XLFN_FRAMEWORK_EXPORTS"));
        assert!(expanded.contains("__xlfn_async_exports"));
        assert!(expanded.contains("__xlfn_rtd_exports"));
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
        assert!(func_expanded.contains("my_custom_xlfn :: __private :: v1"));
        assert!(!func_expanded.contains("my_custom_xlfn :: convert"));

        let addin_item: ItemStruct = syn::parse2(quote!(
            struct MyAddin;
        ))
        .unwrap();
        let addin_expanded = expand_excel_addin(quote!(crate = my_custom_xlfn), addin_item)
            .unwrap()
            .to_string();
        assert!(
            addin_expanded.contains("my_custom_xlfn :: __private :: v1 :: open_generated_addin")
        );
        assert!(addin_expanded.contains("my_custom_xlfn :: __private :: v1 :: MacroRuntime"));
    }
}
