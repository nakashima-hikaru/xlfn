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
#[cfg(test)]
use syn::ItemFn;
use syn::ext::IdentExt;
use syn::{DeriveInput, ItemStruct, parse_macro_input};
mod codegen;
mod function;

use function::UdfFunction;
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
/// trait implementations. Recognized `f64`, `bool`, `i32`, `String`, and
/// synchronous `&str` arguments use Excel's native scalar ABIs and accept Excel's
/// coercion rules. `#[excel_arg(strict)]` selects XLOPER12 with the type's strict
/// conversion instead. Explicit blank, missing, or default policies also select
/// XLOPER12 so the original input's presence is available. Other types and
/// unrecognized aliases use their ordinary conversion.
/// Synchronous `Matrix<f64>`, `Row<f64>`, `Column<f64>`, and
/// `NumericArrayOutput` results use the FP12 (`K%`) result ABI; `Result<T, E>`
/// and `XllResult<T>` inherit this choice. Unrecognized return aliases use
/// XLOPER12 (`Q`). `return_abi = "fp12"` explicitly selects FP12 for a numeric
/// array alias; incompatible resolved types fail to compile.
/// `return_abi = "xloper12"` preserves individual Excel error values such as
/// `#N/A`. FP12 failures return `#NUM!`, with details in the runtime's diagnostic
/// history. Async results always use Excel's asynchronous completion API.
/// An `async fn` selects asynchronous mode directly;
/// there is no `#[excel_function(async)]` mode flag. A standard context type
/// (`MainThreadContext`, `ThreadSafeContext`, `MacroSheetContext`, or
/// `AsyncContext`) is injected when passed by value as the first parameter.
/// Its final path identifier selects the context; ordinary imports and
/// qualified paths work, while type aliases and renamed imports do not select
/// injection. `AsyncContext` is permitted only on an `async fn`.
/// `thread_safe` and `macro_sheet` select execution only for functions without
/// an injected context. Either flag combined with a context or an `async fn`
/// is rejected.
/// `#[excel_arg(default = expression)]` defaults omitted arguments; blank cells
/// keep the parameter type's ordinary conversion unless `blank = default` is
/// selected. Explicit `missing` policies override this inferred default.
/// Both identifier policies (`blank = error`) and string policies are accepted.
/// Use `missing = convert` to keep the declared type's ordinary conversion,
/// including `None` for an omitted `Option<T>` input.
#[proc_macro_attribute]
pub fn excel_function(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as UdfFunction);
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

/// Derives string input and output conversion for a fieldless enum.
///
/// Use `#[excel_enum(ascii_case_insensitive)]` on the enum to accept ASCII
/// spelling variations, and `#[excel_enum(name = "...")]` on a variant to
/// override its worksheet text. Unknown inputs preserve the argument name,
/// received text, and accepted choices in bounded diagnostic detail.
#[proc_macro_derive(ExcelEnum, attributes(excel_enum))]
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
    function: UdfFunction,
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
    let display_name = options.name.unwrap_or_else(|| ident.unraw().to_string());
    let id = options
        .id
        .unwrap_or_else(|| ident.unraw().to_string().to_ascii_lowercase());
    let category = options.category.unwrap_or_else(|| display_name.clone());
    let runtime_constructor = if options.physical_unload {
        quote!(#krate::__private::v1::MacroRuntime::new_with_physical_unload())
    } else {
        quote!(#krate::__private::v1::MacroRuntime::new())
    };
    let unload_policy = if options.physical_unload {
        quote!(#krate::__private::v1::PhysicalUnload)
    } else {
        quote!(#krate::__private::v1::LogicalUnload)
    };
    validate_addin_metadata(&display_name, &id, &category, &item)?;
    let name_units: Vec<_> = display_name.encode_utf16().collect();
    let name_length =
        u16::try_from(name_units.len()).expect("validated Excel metadata string length");
    Ok(quote! {
        #item

        #(#gating)*
        #[doc(hidden)]
        static __XLFN_RUNTIME: #krate::__private::v1::MacroRuntime<
            #ident,
            #unload_policy,
        > = #runtime_constructor;

        #(#gating)*
        #[doc(hidden)]
        static __XLFN_METADATA: #krate::__private::v1::AddinMetadata =
            #krate::__private::v1::AddinMetadata::new(&[#name_length, #(#name_units),*]);

        #(#gating)*
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_FRAMEWORK_EXPORTS: [u8; b"xlAutoOpen\0xlAutoClose\0xlAutoRemove\0xlAutoFree12\0xlAddInManagerInfo12\0".len()] =
            *b"xlAutoOpen\0xlAutoClose\0xlAutoRemove\0xlAutoFree12\0xlAddInManagerInfo12\0";

        #(#gating)*
        #krate::__private::v1::__xlfn_async_exports!(&crate::__XLFN_RUNTIME);

        #(#gating)*
        #krate::__private::v1::__xlfn_calculation_exports!(&crate::__XLFN_RUNTIME);

        #(#gating)*
        #krate::__private::v1::__xlfn_excel_rtd_exports!(&crate::__XLFN_RUNTIME);

        /// Initializes the add-in from Excel's lifecycle entrypoint.
        ///
        /// # Safety
        /// Excel must invoke this on its lifecycle thread while lifecycle
        /// callbacks are permitted; do not call it to renew UDF callback authority.
        #(#gating)*
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAutoOpen() -> i32 {
            #krate::__private::v1::export_status_boundary(0, || {
                // SAFETY: Excel invokes this lifecycle entrypoint with callback authority.
                unsafe {
                    #krate::__private::v1::open_generated_addin::<#ident>(
                        &crate::__XLFN_RUNTIME,
                        #id,
                        #display_name,
                        #category,
                        ::core::env!("CARGO_PKG_VERSION"),
                        #krate::__private::v1::BUILD_TARGET,
                        xlAutoOpen as *const (),
                    )
                }
            })
        }

        /// Deactivates the add-in from Excel's lifecycle entrypoint.
        ///
        /// # Safety
        /// Excel must invoke this on its lifecycle thread while lifecycle
        /// callbacks are permitted; do not call it to renew UDF callback authority.
        #(#gating)*
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAutoClose() -> i32 {
            #krate::__private::v1::export_status_boundary(0, || {
                // SAFETY: Excel invokes this lifecycle entrypoint with callback authority.
                unsafe {
                    #krate::__private::v1::auto_close_generated_addin::<#ident>(&crate::__XLFN_RUNTIME)
                }
            })
        }

        /// Removes the add-in from Excel's lifecycle entrypoint.
        ///
        /// # Safety
        /// Excel must invoke this on its lifecycle thread while lifecycle
        /// callbacks are permitted; do not call it to renew UDF callback authority.
        #(#gating)*
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn xlAutoRemove() -> i32 {
            #krate::__private::v1::export_status_boundary(0, || {
                // SAFETY: Excel invokes this lifecycle entrypoint with callback authority.
                unsafe {
                    #krate::__private::v1::auto_remove_generated_addin::<#ident>(&crate::__XLFN_RUNTIME)
                }
            })
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
            #krate::__private::v1::export_void_boundary(|| {
                // SAFETY: Excel passes the live return pointer produced by this XLL.
                unsafe { #krate::__private::v1::free_generated_return(__pointer) };
            });
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
            #krate::__private::v1::export_metadata_boundary(|| {
                // SAFETY: Excel supplies `__action` as a live XLOPER12 for this ABI call.
                unsafe {
                    #krate::__private::v1::addin_manager_info(
                        &crate::__XLFN_METADATA,
                        __action,
                    )
                }
            })
        }

    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn function(source: proc_macro2::TokenStream) -> UdfFunction {
        syn::parse2(source).unwrap()
    }

    #[test]
    fn raw_identifiers_use_unescaped_excel_metadata() {
        let parsed = model::parse_udf(
            quote!(),
            function(quote!(
                fn r#type(r#match: f64) -> f64 {
                    r#match
                }
            )),
        )
        .unwrap();
        let spec = model::analyze(parsed).unwrap();
        assert_eq!(spec.metadata.id.as_str(), "type");
        assert_eq!(spec.metadata.excel_name, "type");
        assert_eq!(spec.arguments[0].excel_name, "match");

        let parsed = model::parse_udf(
            quote!(name = "TYPE.VALUE"),
            function(quote!(
                fn r#type(#[excel_arg(name = "r#match")] r#match: f64) -> f64 {
                    r#match
                }
            )),
        )
        .unwrap();
        let spec = model::analyze(parsed).unwrap();
        assert_eq!(spec.metadata.excel_name, "TYPE.VALUE");
        assert_eq!(spec.arguments[0].excel_name, "r#match");
    }

    fn assert_outer_abi_boundary(item: &ItemFn, expected: &str) {
        assert!(
            item.sig.abi.is_some(),
            "fixture must inspect an actual extern function"
        );
        let [syn::Stmt::Expr(syn::Expr::Call(call), _)] = item.block.stmts.as_slice() else {
            panic!("an exported body must consist only of its outer panic boundary");
        };
        let syn::Expr::Path(path) = call.func.as_ref() else {
            panic!("exported boundary must be called directly");
        };
        assert_eq!(path.path.segments.last().unwrap().ident, expected);
        assert!(matches!(call.args.last(), Some(syn::Expr::Closure(_))));
    }

    #[test]
    fn generated_udf_and_lifecycle_exports_wrap_the_complete_body() {
        for (source, expected) in [
            (
                quote!(
                    fn value() -> f64 {
                        1.0
                    }
                ),
                "export_value_boundary",
            ),
            (
                quote!(
                    async fn value() -> f64 {
                        1.0
                    }
                ),
                "export_void_boundary",
            ),
            (
                quote!(
                    fn value() -> Matrix<f64> {
                        unreachable!()
                    }
                ),
                "export_numeric_boundary",
            ),
        ] {
            let expanded = expand_excel_function(quote!(), function(source)).unwrap();
            let file: syn::File = syn::parse2(expanded).unwrap();
            let exported: Vec<_> = file
                .items
                .iter()
                .filter_map(|item| {
                    if let syn::Item::Fn(function) = item
                        && function.sig.abi.is_some()
                    {
                        Some(function)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(exported.len(), 1);
            assert_outer_abi_boundary(exported[0], expected);
        }

        let expanded = expand_excel_addin(
            quote!(),
            syn::parse_quote!(
                struct TestAddin;
            ),
        )
        .unwrap();
        let file: syn::File = syn::parse2(expanded).unwrap();
        let mut exports = 0;
        for item in &file.items {
            if let syn::Item::Fn(function) = item
                && function.sig.abi.is_some()
            {
                let expected = match function.sig.ident.to_string().as_str() {
                    "xlAutoOpen" | "xlAutoClose" | "xlAutoRemove" => "export_status_boundary",
                    "xlAutoFree12" => "export_void_boundary",
                    "xlAddInManagerInfo12" => "export_metadata_boundary",
                    name => panic!("unexpected generated lifecycle export {name}"),
                };
                assert!(
                    matches!(function.sig.safety, syn::Safety::Unsafe(_)),
                    "Excel exports must require the host entrypoint contract"
                );
                assert_outer_abi_boundary(function, expected);
                exports += 1;
            }
        }
        assert_eq!(exports, 5);
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
        assert!(expanded.contains("prepare_native_f64"));
        assert!(expanded.contains("assert_thread_safe_return"));
        assert!(expanded.contains("assert_volatile_return"));
    }

    #[test]
    fn numeric_return_abi_is_selected_only_from_recognizable_numeric_array_types() {
        use xlfn_common::ReturnAbi;
        for (ty, abi) in [
            (quote!(Matrix<f64>), ReturnAbi::NumericArray12),
            (quote!(Matrix<(f64)>), ReturnAbi::NumericArray12),
            (quote!(Row<f64>), ReturnAbi::NumericArray12),
            (quote!(Column<f64>), ReturnAbi::NumericArray12),
            (
                quote!(xlfn::value::Matrix<::core::primitive::f64>),
                ReturnAbi::NumericArray12,
            ),
            (quote!(xlfn::prelude::Row<f64>), ReturnAbi::NumericArray12),
            (quote!(NumericArrayOutput), ReturnAbi::NumericArray12),
            (
                quote!(xlfn::output::NumericArrayOutput),
                ReturnAbi::NumericArray12,
            ),
            (quote!(XllResult<Matrix<f64>>), ReturnAbi::NumericArray12),
            (
                quote!(xlfn::error::XllResult<Row<f64>>),
                ReturnAbi::NumericArray12,
            ),
            (
                quote!(Result<Column<f64>, CustomError>),
                ReturnAbi::NumericArray12,
            ),
            (
                quote!(std::result::Result<Matrix<f64>, CustomError>),
                ReturnAbi::NumericArray12,
            ),
            (quote!(f64), ReturnAbi::Value12),
            (quote!(bool), ReturnAbi::Value12),
            (quote!(i32), ReturnAbi::Value12),
            (quote!(String), ReturnAbi::Value12),
            (quote!(&str), ReturnAbi::Value12),
            (quote!(Matrix<i32>), ReturnAbi::Value12),
            (quote!(Matrix<FloatAlias>), ReturnAbi::Value12),
            (quote!(Matrix<custom::f64>), ReturnAbi::Value12),
            (quote!(XlArrayOutput), ReturnAbi::Value12),
            (quote!(NumericAlias), ReturnAbi::Value12),
            (quote!(custom::Matrix<f64>), ReturnAbi::Value12),
            (quote!(<Owner as Trait>::Matrix<f64>), ReturnAbi::Value12),
        ] {
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(fn typed() -> #ty { unreachable!() })),
            )
            .unwrap();
            let analyzed = model::analyze(parsed).unwrap();
            assert_eq!(analyzed.return_abi, abi, "{ty}");
            assert_eq!(model::lower(analyzed).return_abi, abi, "{ty}");
        }
    }

    #[test]
    fn numeric_wrapper_and_registration_choose_the_same_return_abi() {
        let expanded = expand_excel_function(
            quote!(thread_safe),
            function(quote!(
                fn prices(spot: f64, strikes: NumericMatrixRef<'_>) -> XllResult<Matrix<f64>> {
                    unreachable!()
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("sync_udf_fp12"));
        assert!(expanded.contains("NumericReturn :: invoke"));
        assert!(expanded.contains("export_numeric_boundary"));
        assert!(expanded.contains("-> * mut :: xlfn :: __private :: v1 :: FP12"));
        assert!(expanded.contains("ReturnAbi :: NumericArray12"));

        let explicit_value = expand_excel_function(
            quote!(thread_safe, return_abi = "xloper12"),
            function(quote!(
                fn prices() -> Matrix<f64> {
                    unreachable!()
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(explicit_value.contains("ReturnAbi :: Value12"));
        assert!(explicit_value.contains("ExcelReturn :: invoke"));
        assert!(explicit_value.contains("export_value_boundary"));
        assert!(!explicit_value.contains("sync_udf_fp12"));

        let explicit_alias = expand_excel_function(
            quote!(return_abi = "fp12"),
            function(quote!(
                fn prices() -> NumericAlias {
                    unreachable!()
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(explicit_alias.contains("NumericReturn :: invoke"));
    }

    #[test]
    fn renamed_framework_paths_keep_numeric_result_selection() {
        let expanded = expand_excel_function(
            quote!(crate = "framework", thread_safe),
            function(quote!(
                fn prices() -> framework::XllResult<framework::value::Matrix<f64>> {
                    unreachable!()
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("framework :: __private :: v1 :: sync_udf_fp12"));
        assert!(expanded.contains("ReturnAbi :: NumericArray12"));
    }

    #[test]
    fn async_numeric_result_uses_async_completion_and_rejects_fp12_override() {
        let expanded = expand_excel_function(
            quote!(),
            function(quote!(
                async fn prices() -> Matrix<f64> {
                    unreachable!()
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("ReturnAbi :: Value12"));
        assert!(expanded.contains("async_udf"));
        assert!(!expanded.contains("sync_udf_fp12"));
        for (options, expected) in [
            (
                quote!(return_abi = "fp12"),
                "only supported for synchronous",
            ),
            (
                quote!(return_abi = "native"),
                "must be \"xloper12\" or \"fp12\"",
            ),
            (
                quote!(return_abi = "fp12", return_abi = "xloper12"),
                "duplicate `return_abi`",
            ),
        ] {
            let error = expand_excel_function(
                options,
                function(quote!(
                    async fn prices() -> Matrix<f64> {
                        unreachable!()
                    }
                )),
            )
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn native_argument_abi_is_selected_from_standard_type_spellings() {
        use xlfn_common::ArgumentAbi;
        for (ty, abi) in [
            (quote!(f64), ArgumentAbi::Float64),
            (quote!(::core::primitive::f64), ArgumentAbi::Float64),
            (quote!(bool), ArgumentAbi::Boolean16),
            (quote!(std::primitive::bool), ArgumentAbi::Boolean16),
            (quote!(i32), ArgumentAbi::Integer32),
            (quote!(String), ArgumentAbi::CountedUtf16),
            (quote!(::std::string::String), ArgumentAbi::CountedUtf16),
            (quote!(alloc::string::String), ArgumentAbi::CountedUtf16),
            (quote!(&str), ArgumentAbi::CountedUtf16),
            (quote!(i64), ArgumentAbi::Value12),
            (quote!(NumericAlias), ArgumentAbi::Value12),
            (quote!(custom::String), ArgumentAbi::Value12),
            (quote!(custom::f64), ArgumentAbi::Value12),
            (quote!(Option<f64>), ArgumentAbi::Value12),
            (quote!(OptionalExcelValue<f64>), ArgumentAbi::Value12),
            (quote!(Matrix<f64>), ArgumentAbi::Value12),
            (quote!(MatrixRef<'_, f64>), ArgumentAbi::Value12),
            (quote!(ExcelError), ArgumentAbi::Value12),
            (quote!(ExcelSerialDate), ArgumentAbi::Value12),
        ] {
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(fn typed(value: #ty) -> f64 { 0.0 })),
            )
            .unwrap();
            let analyzed = model::analyze(parsed).unwrap();
            assert_eq!(analyzed.arguments[0].abi, abi, "{ty}");
            let lowered = model::lower(analyzed);
            assert_eq!(lowered.arguments[0].abi, abi, "{ty}");
        }
    }

    #[test]
    fn strict_and_all_explicit_presence_policies_select_value12() {
        for options in [
            quote!(strict),
            quote!(default = 1.0),
            quote!(blank = error),
            quote!(missing = error),
            quote!(blank = convert),
            quote!(missing = "convert"),
            quote!(strict, blank = error),
        ] {
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(
                    fn typed(#[excel_arg(#options)] value: f64) -> f64 {
                        value
                    }
                )),
            )
            .unwrap();
            let analyzed = model::analyze(parsed).unwrap();
            assert_eq!(analyzed.arguments[0].abi, xlfn_common::ArgumentAbi::Value12);
        }
    }

    #[test]
    fn registration_metadata_does_not_disable_native_abi() {
        let parsed = model::parse_udf(
            quote!(),
            function(quote!(
                fn typed(#[excel_arg(name = "input", description = "value")] value: f64) -> f64 {
                    value
                }
            )),
        )
        .unwrap();
        let analyzed = model::analyze(parsed).unwrap();
        assert_eq!(analyzed.arguments[0].abi, xlfn_common::ArgumentAbi::Float64);
    }

    #[test]
    fn native_abi_drives_ffi_types_and_both_execution_conversions() {
        for asynchronous in [false, true] {
            let qualifier = asynchronous.then(|| quote!(async));
            let expanded = expand_excel_function(
                quote!(),
                function(quote!(
                    #qualifier fn typed(
                        number: f64,
                        enabled: bool,
                        count: i32,
                        text: String,
                        #[excel_arg(strict)] strict_number: f64,
                    ) -> f64 { number }
                )),
            )
            .unwrap();
            let file: syn::File = syn::parse2(expanded.clone()).unwrap();
            let wrapper = file
                .items
                .iter()
                .find_map(|item| match item {
                    syn::Item::Fn(function) if function.sig.abi.is_some() => Some(function),
                    _ => None,
                })
                .unwrap();
            let types = wrapper
                .sig
                .inputs
                .iter()
                .map(|argument| match argument {
                    syn::FnArg::Typed(argument) => quote!(#argument).to_string(),
                    _ => panic!("generated free function"),
                })
                .collect::<Vec<_>>();
            assert_eq!(types[0], "__raw_0 : :: core :: primitive :: f64");
            assert_eq!(types[1], "__raw_1 : :: core :: primitive :: i16");
            assert_eq!(types[2], "__raw_2 : :: core :: primitive :: i32");
            assert_eq!(types[3], "__raw_3 : * const :: core :: primitive :: u16");
            assert_eq!(
                types[4],
                "__raw_4 : * mut :: xlfn :: __private :: v1 :: XLOPER12"
            );
            assert_eq!(types.len(), if asynchronous { 6 } else { 5 });
            let expanded = expanded.to_string();
            for variant in [
                "Float64",
                "Boolean16",
                "Integer32",
                "CountedUtf16",
                "Value12",
            ] {
                assert!(expanded.contains(&format!("ArgumentAbi :: {variant}")));
            }
            for suffix in ["f64", "bool", "i32", "string"] {
                let prefix = if asynchronous { "" } else { "prepare_" };
                assert!(expanded.contains(&format!("{prefix}native_{suffix}")));
            }
        }
    }

    #[test]
    fn strict_rejects_special_abis_and_duplicates() {
        for (options, ty) in [
            (quote!(strict), quote!(NumericMatrixRef<'_>)),
            (quote!(strict, reference), quote!(ExcelReference<'_>)),
            (quote!(strict, strict), quote!(f64)),
            (quote!(coerce), quote!(f64)),
            (quote!(abi = "B"), quote!(f64)),
        ] {
            assert!(
                expand_excel_function(
                    quote!(macro_sheet),
                    function(quote!(
                        fn typed(#[excel_arg(#options)] value: #ty) -> f64 { 0.0 }
                    ))
                )
                .is_err()
            );
        }
        let error = expand_excel_function(
            quote!(),
            function(quote!(
                async fn typed(value: &str) -> f64 {
                    0.0
                }
            )),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only supported by synchronous functions")
        );
    }

    #[test]
    fn numeric_matrix_registers_and_receives_fp12() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.NUMERIC.MATRIX", thread_safe),
            function(quote!(
                fn numeric_matrix(values: xlfn::value::NumericMatrixRef<'_>, factor: f64) -> f64 {
                    values.iter().copied().sum::<f64>() * factor
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("ArgumentAbi :: NumericArray12"));
        assert!(expanded.contains("* const :: xlfn :: __private :: v1 :: FP12"));
        assert!(expanded.contains("prepare_numeric_matrix"));
        assert!(expanded.contains("prepare_native_f64"));
    }

    #[test]
    fn numeric_matrix_rejects_async_and_presence_policies() {
        for source in [
            quote!(
                async fn numeric_matrix(values: NumericMatrixRef<'_>) -> f64 {
                    0.0
                }
            ),
            quote!(
                fn numeric_matrix(#[excel_arg(blank = error)] values: NumericMatrixRef<'_>) -> f64 {
                    0.0
                }
            ),
            quote!(
                fn numeric_matrix(values: &NumericMatrixRef<'_>) -> f64 {
                    0.0
                }
            ),
        ] {
            assert!(expand_excel_function(quote!(), function(source)).is_err());
        }
    }

    #[test]
    fn scoped_argument_types_reject_invalid_shapes_and_static_lifetimes() {
        for (ty, asynchronous, expected) in [
            (
                quote!(NumericMatrixRef<'static>),
                false,
                "NumericMatrixRef cannot use the 'static lifetime",
            ),
            (
                quote!(NumericMatrixRef<f64>),
                false,
                "NumericMatrixRef accepts only its call lifetime",
            ),
            (
                quote!(NumericMatrixRef<'_, f64>),
                false,
                "NumericMatrixRef accepts only its call lifetime",
            ),
            (
                quote!(<Owner as Trait>::NumericMatrixRef<'_>),
                false,
                "NumericMatrixRef cannot use qualified associated type paths",
            ),
            (
                quote!(&HandleLease<'_, Dataset>),
                true,
                "HandleLease must be passed by value",
            ),
            (
                quote!(*const HandleLease<'_, Dataset>),
                true,
                "HandleLease must be passed by value",
            ),
            (
                quote!(HandleLease<'static, Dataset>),
                true,
                "HandleLease cannot use the 'static lifetime",
            ),
            (
                quote!(<Owner as Trait>::HandleLease<'_, Dataset>),
                true,
                "HandleLease cannot use qualified associated type paths",
            ),
            (
                quote!(MainThreadContext<'static, State>),
                false,
                "injected contexts cannot use the 'static lifetime",
            ),
            (
                quote!(AsyncContext<'static, State>),
                true,
                "injected contexts cannot use the 'static lifetime",
            ),
        ] {
            let asynchronous = asynchronous.then(|| quote!(async));
            let error = expand_excel_function(
                quote!(),
                function(quote!(#asynchronous fn value(input: #ty) -> f64 { 1.0 })),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn grouped_scoped_arguments_keep_their_special_conversion() {
        for (ty, asynchronous, expected) in [
            (
                quote!(NumericMatrixRef<'_>),
                false,
                "ArgumentAbi :: NumericArray12",
            ),
            (
                quote!(NumericMatrixRef),
                false,
                "ArgumentAbi :: NumericArray12",
            ),
            (
                quote!(HandleLease<'_, Dataset>),
                true,
                "PendingHandleLease < Dataset >",
            ),
        ] {
            let ty = proc_macro2::Group::new(proc_macro2::Delimiter::None, ty);
            let asynchronous = asynchronous.then(|| quote!(async));
            let expanded = expand_excel_function(
                quote!(),
                function(quote!(#asynchronous fn value(input: #ty) -> f64 { 1.0 })),
            )
            .unwrap()
            .to_string();
            assert!(expanded.contains(expected), "{expanded}");
        }
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
    fn registration_metadata_is_validated_before_codegen() {
        let too_long = "😀".repeat(xlfn_common::EXCEL_STRING_LIMIT / 2 + 1);
        for value in ["embedded\0nul", too_long.as_str()] {
            for key in ["name", "category", "description", "help_topic"] {
                let key = syn::Ident::new(key, proc_macro2::Span::call_site());
                let error = expand_excel_function(
                    quote!(#key = #value),
                    function(quote!(
                        fn metadata() -> f64 {
                            0.0
                        }
                    )),
                )
                .unwrap_err();
                assert!(error.to_string().starts_with("invalid Excel function"));
            }
        }

        let maximum = "x".repeat(xlfn_common::EXCEL_STRING_LIMIT);
        assert!(
            expand_excel_function(
                quote!(description = #maximum),
                function(quote!(
                    fn metadata() -> f64 {
                        0.0
                    }
                )),
            )
            .is_ok()
        );
    }

    #[test]
    fn argument_descriptions_use_the_runtime_counted_string_rules() {
        let too_long = "x".repeat(xlfn_common::EXCEL_STRING_LIMIT + 1);
        for description in ["embedded\0nul", too_long.as_str()] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(
                    fn metadata(#[excel_arg(description = #description)] value: f64) -> f64 {
                        value
                    }
                )),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("invalid Excel argument description")
            );
        }
    }

    #[test]
    fn addin_metadata_rejects_nul_and_shared_windows_device_names() {
        for attributes in [
            quote!(name = "embedded\0nul"),
            quote!(category = "embedded\0nul"),
            quote!(id = "con"),
            quote!(id = "lPt9"),
        ] {
            let item = syn::parse_quote!(
                struct TestAddin;
            );
            assert!(expand_excel_addin(attributes, item).is_err());
        }
    }

    #[test]
    fn default_implies_missing_policy_without_changing_blank_conversion() {
        let parsed = model::parse_udf(
            quote!(),
            function(quote!(
                fn func(#[excel_arg(default = 1.0)] arg: f64) -> f64 {
                    arg
                }
            )),
        )
        .unwrap();
        let analyzed = model::analyze(parsed).unwrap();
        let model::ArgumentConversion::Value(value) = &analyzed.arguments[0].conversion else {
            panic!("scalar value conversion expected");
        };
        assert!(matches!(value.blank, model::PresenceAction::Convert));
        assert!(matches!(value.missing, model::PresenceAction::Default(_)));
    }

    #[test]
    fn default_respects_explicit_presence_overrides_and_rejects_unused_defaults() {
        let parsed = model::parse_udf(
            quote!(),
            function(quote!(
                fn func(
                    #[excel_arg(default = 1.0, blank = default, missing = error)] arg: f64,
                ) -> f64 {
                    arg
                }
            )),
        )
        .unwrap();
        let analyzed = model::analyze(parsed).unwrap();
        let model::ArgumentConversion::Value(value) = &analyzed.arguments[0].conversion else {
            panic!("scalar value conversion expected");
        };
        assert!(matches!(value.blank, model::PresenceAction::Default(_)));
        assert!(matches!(value.missing, model::PresenceAction::Error));

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
    fn explicit_convert_preserves_typed_presence_conversion_with_a_blank_default() {
        for policy in [quote!(convert), quote!("convert"), quote!(r#convert)] {
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(
                    fn func(
                        #[excel_arg(default = Some(1.0), blank = default, missing = #policy)]
                        arg: Option<f64>,
                    ) -> f64 {
                        arg.unwrap_or(0.0)
                    }
                )),
            )
            .unwrap();
            let analyzed = model::analyze(parsed).unwrap();
            let model::ArgumentConversion::Value(value) = &analyzed.arguments[0].conversion else {
                panic!("scalar value conversion expected");
            };
            assert!(matches!(value.blank, model::PresenceAction::Default(_)));
            assert!(matches!(value.missing, model::PresenceAction::Convert));
        }
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
                #[excel_enum(name = "P")]
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
        assert!(
            expanded
                .contains("Self : :: core :: marker :: Send + :: core :: marker :: Sync + 'static")
        );
    }

    #[test]
    fn excel_enum_rejects_empty_enums_before_codegen() {
        let input: DeriveInput = syn::parse2(quote!(
            enum Empty {}
        ))
        .unwrap();
        let error = expand_excel_enum(input).unwrap_err();
        assert_eq!(error.to_string(), "ExcelEnum requires at least one variant");
    }

    #[test]
    fn context_and_return_type_drive_registration_and_boundary() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.VALUE"),
            function(quote!(
                /// Function Wizard help.
                fn value(context: ThreadSafeContext<'_, State>) -> i32 {
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
    fn standard_context_types_select_execution_and_are_not_excel_arguments() {
        use xlfn_common::ExecutionKind;
        for (is_async, context, kind) in [
            (
                false,
                quote!(MainThreadContext<'_, State>),
                ExecutionKind::MainThread,
            ),
            (
                false,
                quote!(ThreadSafeContext<'_, State>),
                ExecutionKind::ThreadSafe,
            ),
            (
                false,
                quote!(MacroSheetContext<'_, State>),
                ExecutionKind::MacroSheet,
            ),
            (true, quote!(AsyncContext<'_, State>), ExecutionKind::Async),
            (
                false,
                quote!(xlfn::MainThreadContext<'_, State>),
                ExecutionKind::MainThread,
            ),
            (
                false,
                quote!(::xlfn::ThreadSafeContext<'_, State>),
                ExecutionKind::ThreadSafe,
            ),
            (
                false,
                quote!(crate::contexts::MacroSheetContext<'_, State>),
                ExecutionKind::MacroSheet,
            ),
            (
                true,
                quote!(::xlfn::AsyncContext<'_, State>),
                ExecutionKind::Async,
            ),
            (
                false,
                quote!(MainThreadContext<State>),
                ExecutionKind::MainThread,
            ),
            (
                false,
                quote!(ThreadSafeContext<State>),
                ExecutionKind::ThreadSafe,
            ),
            (
                false,
                quote!(MacroSheetContext<State>),
                ExecutionKind::MacroSheet,
            ),
            (true, quote!(AsyncContext<State>), ExecutionKind::Async),
        ] {
            let asyncness = is_async.then(|| quote!(async));
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(
                    #asyncness fn value(context: #context, input: f64) -> f64 { input }
                )),
            )
            .unwrap();
            let spec = model::analyze(parsed).unwrap();
            assert_eq!(spec.execution.kind(), kind);
            assert!(spec.execution.context_type().is_some());
            assert_eq!(spec.arguments.len(), 1);
            assert_eq!(spec.arguments[0].excel_name, "input");
        }
    }

    #[test]
    fn aliases_renamed_imports_and_custom_types_are_excel_arguments() {
        for context in [
            quote!(MainContext<'_>),
            quote!(Context<'_, State>),
            quote!(CustomContext),
            quote!(RenamedAsyncContext<'_, State>),
        ] {
            let parsed = model::parse_udf(
                quote!(),
                function(quote!(fn value(context: #context) -> f64 { 1.0 })),
            )
            .unwrap();
            let spec = model::analyze(parsed).unwrap();
            assert!(spec.execution.context_type().is_none());
            assert_eq!(spec.arguments.len(), 1);
            assert_eq!(spec.arguments[0].excel_name, "context");
        }
    }

    #[test]
    fn execution_mode_flags_reject_every_injected_context() {
        for (asyncness, context) in [
            (quote!(), quote!(MainThreadContext<'_, State>)),
            (quote!(), quote!(ThreadSafeContext<'_, State>)),
            (quote!(), quote!(MacroSheetContext<'_, State>)),
            (quote!(async), quote!(AsyncContext<'_, State>)),
        ] {
            for flag in [
                quote!(thread_safe),
                quote!(macro_sheet),
                quote!(thread_safe, macro_sheet),
            ] {
                let error = expand_excel_function(
                    flag,
                    function(quote!(#asyncness fn value(context: #context) -> f64 { 1.0 })),
                )
                .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    "thread_safe and macro_sheet cannot be combined with an injected context"
                );
            }
        }
    }

    #[test]
    fn async_function_uses_native_async_boundary_and_borrowed_context() {
        let expanded = expand_excel_function(
            quote!(name = "TEST.ASYNC"),
            function(quote!(
                async fn value(context: AsyncContext<'_, State>, input: f64) -> XllResult<f64> {
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
    fn asyncness_requires_the_matching_context_type() {
        for context in [
            quote!(MainThreadContext<'_, State>),
            quote!(ThreadSafeContext<'_, State>),
            quote!(MacroSheetContext<'_, State>),
        ] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(async fn value(context: #context) -> f64 { 1.0 })),
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "an async Excel function can only inject AsyncContext"
            );
        }
        let error = expand_excel_function(
            quote!(),
            function(quote!(
                fn value(context: AsyncContext<'_, State>) -> f64 {
                    1.0
                }
            )),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "AsyncContext can only be injected by an async Excel function"
        );
    }

    #[test]
    fn async_functions_reject_synchronous_execution_flags_without_context() {
        for (flag, expected) in [
            (
                quote!(thread_safe),
                "an async Excel function cannot be marked thread_safe",
            ),
            (
                quote!(macro_sheet),
                "an async Excel function cannot be a macro-sheet function",
            ),
        ] {
            let error = expand_excel_function(
                flag,
                function(quote!(
                    async fn value(input: f64) -> f64 {
                        input
                    }
                )),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn contexts_require_one_first_parameter_by_value_with_standard_generics() {
        for (parameters, expected) in [
            (
                quote!(input: f64, context: MainThreadContext<'_, State>),
                "an injected context must be the first argument",
            ),
            (
                quote!(context: MainThreadContext<'_, State>, another: ThreadSafeContext<'_, State>),
                "only one injected context parameter is allowed",
            ),
            (
                quote!(context: &MainThreadContext<'_, State>),
                "injected contexts must be passed by value",
            ),
            (
                quote!(context: &mut MacroSheetContext<'_, State>),
                "injected contexts must be passed by value",
            ),
            (
                quote!(context: *const ThreadSafeContext<'_, State>),
                "injected contexts must be passed by value",
            ),
            (
                quote!(context: <Owner as Trait>::MainThreadContext<'_, State>),
                "injected contexts cannot use qualified associated type paths",
            ),
            (
                quote!(#[excel_arg(name = "context")] context: MainThreadContext<'_, State>),
                "injected contexts cannot use #[excel_arg(...)]",
            ),
        ] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(fn value(#parameters) -> f64 { 1.0 })),
            )
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
        for context in [
            quote!(MainThreadContext),
            quote!(ThreadSafeContext<'_, State, Extra>),
            quote!(MacroSheetContext<'_>),
            quote!(AsyncContext<State, '_, Extra>),
        ] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(fn value(context: #context) -> f64 { 1.0 })),
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "injected context types must specify an add-in type, optionally preceded by a lifetime"
            );
        }
    }

    #[test]
    fn excel_context_attributes_are_rejected_without_role_parsing() {
        for attribute in [
            quote!(#[excel_context(main_thread)]),
            quote!(#[excel_context(asynchronous)]),
            quote!(#[excel_context]),
            quote!(#[excel_context(unknown, extra)]),
        ] {
            let error = expand_excel_function(
                quote!(),
                function(quote!(fn value(#attribute context: MainThreadContext<'_, State>) -> f64 { 1.0 })),
            ).unwrap_err();
            assert_eq!(
                error.to_string(),
                "#[excel_context(...)] is not supported; use a standard context type as the first parameter"
            );
        }
    }

    #[test]
    fn scoped_handle_async_builder_constructs_the_standard_context() {
        let expanded = expand_excel_function(
            quote!(),
            function(quote!(
                async fn value(
                    context: xlfn::AsyncContext<'_, State>,
                    object: HandleLease<'_, Dataset>,
                ) -> f64 {
                    1.0
                }
            )),
        )
        .unwrap()
        .to_string();
        assert!(expanded.contains("ExecutionLease < State >"));
        assert!(expanded.contains("async_context :: < State >"));
        assert!(expanded.contains("let __context : xlfn :: AsyncContext"));
        assert!(!expanded.contains("AsyncContextFactory"));
        assert!(!expanded.contains("async_context_for"));
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
                fn bad(context: MacroSheetContext<'_, State>) -> XllResult<i32> {
                    Ok(1)
                }
            )),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "thread_safe and macro_sheet cannot be combined with an injected context"
        );

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
        for signature in [
            quote!(
                fn gated() -> f64 {
                    1.0
                }
            ),
            quote!(
                async fn gated() -> f64 {
                    1.0
                }
            ),
        ] {
            let expanded = expand_excel_function(
                quote!(name = "TEST.GATED"),
                function(quote! {
                    #[cfg(feature = "gated")]
                    #signature
                }),
            )
            .unwrap();
            assert_generated_items_are_gated(
                expanded,
                &[
                    "gated",
                    "__XLFN_DESCRIPTOR_gated",
                    "submit_registration",
                    "__XLFN_EXP_gated",
                    "xll_gated",
                ],
            );
        }
    }

    fn assert_generated_items_are_gated(
        expanded: proc_macro2::TokenStream,
        expected_names: &[&str],
    ) {
        let file: syn::File = syn::parse2(expanded).unwrap();
        let expected: syn::Attribute = syn::parse_quote!(#[cfg(feature = "gated")]);
        let expected = quote!(#expected).to_string();
        let mut names = Vec::new();
        for item in file.items {
            let (name, attributes) = match item {
                syn::Item::Fn(item) => (item.sig.ident.to_string(), item.attrs),
                syn::Item::Struct(item) => (item.ident.to_string(), item.attrs),
                syn::Item::Static(item) => (item.ident.to_string(), item.attrs),
                syn::Item::Macro(item) => (
                    item.mac.path.segments.last().unwrap().ident.to_string(),
                    item.attrs,
                ),
                item => panic!("uninspected generated item: {}", quote!(#item)),
            };
            assert!(
                attributes
                    .iter()
                    .any(|attribute| quote!(#attribute).to_string() == expected),
                "generated item {name} is missing its cfg gate"
            );
            names.push(name);
        }
        names.sort_unstable();
        let mut expected_names = expected_names.to_vec();
        expected_names.sort_unstable();
        assert_eq!(names, expected_names);
    }

    #[test]
    fn excel_addin_propagates_cfg_to_lifecycle_exports() {
        let item: ItemStruct = syn::parse2(quote! {
            #[cfg(feature = "gated")]
            struct GatedAddin;
        })
        .unwrap();
        let expanded = expand_excel_addin(quote!(), item).unwrap();
        assert_generated_items_are_gated(
            expanded,
            &[
                "GatedAddin",
                "__XLFN_RUNTIME",
                "__XLFN_METADATA",
                "__XLFN_FRAMEWORK_EXPORTS",
                "__xlfn_async_exports",
                "__xlfn_calculation_exports",
                "__xlfn_excel_rtd_exports",
                "xlAutoOpen",
                "xlAutoClose",
                "xlAutoRemove",
                "xlAutoFree12",
                "xlAddInManagerInfo12",
            ],
        );
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
        assert!(expanded.contains("prepare_argument"));
        assert!(
            expanded.find("prepare_argument").unwrap()
                < expanded.find("ExcelReturn :: invoke").unwrap()
        );
        assert!(
            expanded.find("ExcelReturn :: invoke").unwrap() < expanded.find("materialize").unwrap()
        );
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
        assert!(expanded.contains("prepare_default"));
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
        assert!(expanded.contains("__xlfn_excel_rtd_exports"));
        assert!(!expanded.contains("fn __xlfn_calculation_canceled"));
        assert!(expanded.contains("LogicalUnload"));
        assert!(!expanded.contains("PhysicalUnload"));
    }

    #[test]
    fn physical_unload_requires_the_explicit_runtime_constructor() {
        let item: ItemStruct = syn::parse2(quote! {
            pub struct UnloadableAddin;
        })
        .unwrap();
        let expanded = expand_excel_addin(quote!(physical_unload), item)
            .unwrap()
            .to_string();
        assert!(expanded.contains("new_with_physical_unload"));
        assert!(expanded.contains("PhysicalUnload"));
        assert!(!expanded.contains("LogicalUnload"));
        assert!(!expanded.contains("MacroRuntime :: new ()"));
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
