//! The UDF front end: parsed syntax, semantic analysis, and lowering.
//!
//! This module intentionally contains no generated TokenStream. Raw attribute
//! values are accepted by the parser, normalized into semantic enums by the
//! analyzer, and only then lowered into names needed by the emitter.

use crate::function::UdfFunction;
use crate::options::{
    ParsedArgumentOptions, ParsedFunctionOptions, parse_argument_options, parse_function_options,
};
use crate::support::{doc_comment, extract_gating_attributes, resolve_crate_path};
use crate::validation::{validate_export_id, validate_registration_string};
use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident};
use syn::ext::IdentExt;
use syn::{Attribute, Expr, FnArg, GenericArgument, Ident, Pat, Path, PathArguments, Type};

/// Syntax extracted from one excel_function item.
pub(super) struct ParsedUdf {
    pub(super) function: UdfFunction,
    pub(super) options: ParsedFunctionOptions,
    pub(super) context: Option<ParsedContext>,
    pub(super) arguments: Vec<ParsedArgument>,
    pub(super) gating: Vec<Attribute>,
}

pub(super) struct ParsedContext {
    kind: ContextKind,
    pub(super) ty: Type,
}

#[derive(Clone, Copy)]
enum ContextKind {
    MainThread,
    ThreadSafe,
    MacroSheet,
    Async,
}

/// Context injection is syntax: Rust name resolution and aliases are outside
/// the proc macro's view. Inspect wrapped paths too, so recognizable borrowed
/// or associated context types receive a deterministic error below.
fn context_type_path(ty: &Type) -> Option<&syn::TypePath> {
    match ty {
        Type::Path(path) => Some(path),
        Type::Reference(reference) => context_type_path(&reference.elem),
        Type::Ptr(pointer) => context_type_path(&pointer.elem),
        Type::Paren(paren) => context_type_path(&paren.elem),
        Type::Group(group) => context_type_path(&group.elem),
        _ => None,
    }
}

fn recognize_context_type(ty: &Type) -> Option<ContextKind> {
    let ident = &context_type_path(ty)?.path.segments.last()?.ident;
    match ident.unraw().to_string().as_str() {
        "MainThreadContext" => Some(ContextKind::MainThread),
        "ThreadSafeContext" => Some(ContextKind::ThreadSafe),
        "MacroSheetContext" => Some(ContextKind::MacroSheet),
        "AsyncContext" => Some(ContextKind::Async),
        _ => None,
    }
}

fn is_context_value_path(ty: &Type) -> bool {
    match ty {
        Type::Path(_) => true,
        Type::Paren(paren) => is_context_value_path(&paren.elem),
        Type::Group(group) => is_context_value_path(&group.elem),
        _ => false,
    }
}

pub(super) fn context_addin_type(ty: &Type) -> syn::Result<&Type> {
    let arguments = context_type_path(ty)
        .and_then(|path| path.path.segments.last())
        .and_then(|segment| match &segment.arguments {
            PathArguments::AngleBracketed(arguments) => Some(&arguments.args),
            _ => None,
        });
    if let Some(arguments) = arguments {
        let mut entries = arguments.iter();
        match (entries.next(), entries.next(), entries.next()) {
            (Some(GenericArgument::Type(addin)), None, None) => {
                return Ok(addin);
            }
            (
                Some(GenericArgument::Lifetime(lifetime)),
                Some(GenericArgument::Type(addin)),
                None,
            ) => {
                if lifetime.ident == "static" {
                    return Err(syn::Error::new_spanned(
                        lifetime,
                        "injected contexts cannot use the 'static lifetime",
                    ));
                }
                return Ok(addin);
            }
            _ => {}
        }
    }
    Err(syn::Error::new_spanned(
        ty,
        "injected context types must specify an add-in type, optionally preceded by a lifetime",
    ))
}

// Unlike synchronous signatures, Rust async signatures cannot implicitly
// elide a lifetime in a type path (E0726). The recognized context syntax
// permits omission, so make the call lifetime explicit before emitting it.
fn normalize_async_context_lifetime(ty: &mut Type) {
    match ty {
        Type::Paren(paren) => normalize_async_context_lifetime(&mut paren.elem),
        Type::Group(group) => normalize_async_context_lifetime(&mut group.elem),
        Type::Path(path) => {
            let Some(segment) = path.path.segments.last_mut() else {
                return;
            };
            if let PathArguments::AngleBracketed(arguments) = &mut segment.arguments
                && arguments.args.len() == 1
                && matches!(arguments.args.first(), Some(GenericArgument::Type(_)))
            {
                arguments.args.insert(0, syn::parse_quote!('_));
            }
        }
        _ => {}
    }
}

pub(super) struct ParsedArgument {
    pub(super) rust_name: Ident,
    pub(super) ty: Type,
    pub(super) options: ParsedArgumentOptions,
}

/// A single normalized execution capability. Its variants make incompatible
/// mode/context combinations unrepresentable after semantic analysis.
pub(super) enum ExecutionSpec {
    MainThread { context_ty: Option<Type> },
    ThreadSafe { context_ty: Option<Type> },
    MacroSheet { context_ty: Option<Type> },
    Async { context_ty: Option<Type> },
}

impl ExecutionSpec {
    pub(super) const fn kind(&self) -> xlfn_common::ExecutionKind {
        match self {
            Self::MainThread { .. } => xlfn_common::ExecutionKind::MainThread,
            Self::ThreadSafe { .. } => xlfn_common::ExecutionKind::ThreadSafe,
            Self::MacroSheet { .. } => xlfn_common::ExecutionKind::MacroSheet,
            Self::Async { .. } => xlfn_common::ExecutionKind::Async,
        }
    }

    pub(super) const fn is_async(&self) -> bool {
        matches!(self, Self::Async { .. })
    }

    pub(super) const fn is_macro_sheet(&self) -> bool {
        matches!(self, Self::MacroSheet { .. })
    }

    pub(super) fn context_type(&self) -> Option<&Type> {
        match self {
            Self::MainThread { context_ty }
            | Self::ThreadSafe { context_ty }
            | Self::MacroSheet { context_ty }
            | Self::Async { context_ty } => context_ty.as_ref(),
        }
    }
}

/// The semantic action for blank or missing input.
#[derive(Clone)]
pub(super) enum PresenceAction {
    Convert,
    Error,
    Default(Expr),
}

impl PresenceAction {
    pub(super) const fn requires_presence_check(&self) -> bool {
        !matches!(self, Self::Convert)
    }
}

/// A conversion is either an ordinary Excel value conversion with normalized
/// presence actions, a raw reference conversion, or an async-scoped handle
/// lease conversion.
pub(super) enum ArgumentConversion {
    Value(Box<ValueConversion>),
    Reference,
    NumericMatrix,
    HandleLease { object: Box<Type> },
}

pub(super) struct ValueConversion {
    pub(super) blank: PresenceAction,
    pub(super) missing: PresenceAction,
}

impl ArgumentConversion {
    pub(super) const fn is_reference(&self) -> bool {
        matches!(self, Self::Reference)
    }

    pub(super) const fn is_handle_lease(&self) -> bool {
        matches!(self, Self::HandleLease { .. })
    }

    pub(super) const fn requires_presence_check(&self) -> bool {
        match self {
            Self::Reference | Self::NumericMatrix | Self::HandleLease { .. } => false,
            Self::Value(value) => {
                value.blank.requires_presence_check() || value.missing.requires_presence_check()
            }
        }
    }
}

/// A validated UDF argument. No raw policy strings remain here.
pub(super) struct ArgumentSpec {
    pub(super) ty: Type,
    pub(super) excel_name: String,
    pub(super) description: String,
    pub(super) abi: xlfn_common::ArgumentAbi,
    pub(super) conversion: ArgumentConversion,
}

pub(super) struct UdfId(String);

impl UdfId {
    fn parse(value: String, span: &impl ToTokens) -> syn::Result<Self> {
        validate_export_id(&value, span)?;
        Ok(Self(value))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) struct UdfMetadata {
    pub(super) id: UdfId,
    pub(super) excel_name: String,
    pub(super) category: String,
    pub(super) description: String,
    pub(super) help_topic: String,
}

/// The semantic meaning of one UDF, before generated identifiers are chosen.
pub(super) struct UdfSpec {
    pub(super) function: UdfFunction,
    pub(super) metadata: UdfMetadata,
    pub(super) execution: ExecutionSpec,
    pub(super) arguments: Vec<ArgumentSpec>,
    pub(super) return_type: Type,
    pub(super) return_abi: xlfn_common::ReturnAbi,
    pub(super) handle_reuse: Option<bool>,
    pub(super) volatile: bool,
    pub(super) hidden: bool,
    pub(super) gating: Vec<Attribute>,
    pub(super) crate_path: Path,
}

pub(super) struct UdfSymbols {
    pub(super) export_ident: Ident,
    pub(super) descriptor_ident: Ident,
    pub(super) manifest_ident: Ident,
    pub(super) export_name_bytes: Vec<u8>,
}

pub(super) struct ArgumentPlan {
    pub(super) index: usize,
    pub(super) ty: Type,
    pub(super) excel_name: String,
    pub(super) description: String,
    pub(super) raw_ident: Ident,
    pub(super) local_ident: Ident,
    pub(super) abi: xlfn_common::ArgumentAbi,
    pub(super) conversion: ArgumentConversion,
}

/// Code-generation-oriented IR. It contains generated symbols, but still no
/// emitted tokens or semantic policy interpretation.
pub(super) struct UdfPlan {
    pub(super) function: UdfFunction,
    pub(super) metadata: UdfMetadata,
    pub(super) execution: ExecutionSpec,
    pub(super) arguments: Vec<ArgumentPlan>,
    pub(super) return_type: Type,
    pub(super) return_abi: xlfn_common::ReturnAbi,
    pub(super) handle_reuse: Option<bool>,
    pub(super) volatile: bool,
    pub(super) hidden: bool,
    pub(super) gating: Vec<Attribute>,
    pub(super) crate_path: Path,
    pub(super) symbols: UdfSymbols,
}

/// Parse only syntax and remove macro-owned argument attributes from the item.
pub(super) fn parse_udf(
    attributes: TokenStream,
    mut function: UdfFunction,
) -> syn::Result<ParsedUdf> {
    let options = parse_function_options(attributes)?;
    let gating = extract_gating_attributes(&function.attrs);
    let mut context = None;
    let mut arguments = Vec::new();

    for (index, input) in function.sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = input else {
            continue;
        };

        let mut retained = Vec::new();
        let mut has_excel_arg = false;
        let mut excel_arg_attribute = None;
        let mut argument_options = ParsedArgumentOptions::default();
        for attribute in std::mem::take(&mut argument.attrs) {
            if attribute.path().is_ident("excel_context") {
                return Err(syn::Error::new_spanned(
                    attribute,
                    "#[excel_context(...)] is not supported; use a standard context type as the first parameter",
                ));
            } else if attribute.path().is_ident("excel_arg") {
                has_excel_arg = true;
                parse_argument_options(&attribute, &mut argument_options)?;
                excel_arg_attribute = Some(attribute);
            } else {
                retained.push(attribute);
            }
        }
        argument.attrs = retained;

        if let Some(kind) = recognize_context_type(&argument.ty) {
            if !is_context_value_path(&argument.ty) {
                return Err(syn::Error::new_spanned(
                    &argument.ty,
                    "injected contexts must be passed by value",
                ));
            }
            if context_type_path(&argument.ty).is_some_and(|path| path.qself.is_some()) {
                return Err(syn::Error::new_spanned(
                    &argument.ty,
                    "injected contexts cannot use qualified associated type paths",
                ));
            }
            context_addin_type(&argument.ty)?;
            if matches!(kind, ContextKind::Async) {
                normalize_async_context_lifetime(&mut argument.ty);
            }
            if context.is_some() {
                return Err(syn::Error::new_spanned(
                    argument,
                    "only one injected context parameter is allowed",
                ));
            }
            if index != 0 {
                return Err(syn::Error::new_spanned(
                    argument,
                    "an injected context must be the first argument",
                ));
            }
            if has_excel_arg {
                if let Some(attribute) = excel_arg_attribute {
                    argument.attrs.push(attribute);
                }
                return Err(syn::Error::new_spanned(
                    argument,
                    "injected contexts cannot use #[excel_arg(...)]",
                ));
            }
            context = Some(ParsedContext {
                kind,
                ty: argument.ty.as_ref().clone(),
            });
        } else {
            let Pat::Ident(pattern) = argument.pat.as_ref() else {
                return Err(syn::Error::new_spanned(
                    &argument.pat,
                    "Excel function arguments must use simple identifier patterns",
                ));
            };
            arguments.push(ParsedArgument {
                rust_name: pattern.ident.clone(),
                ty: argument.ty.as_ref().clone(),
                options: argument_options,
            });
        }
    }

    Ok(ParsedUdf {
        function,
        options,
        context,
        arguments,
        gating,
    })
}

/// Analyze all cross-field constraints and normalize raw options into semantic
/// enums. A successful result contains no contradictory execution or argument
/// policy state.
pub(super) fn analyze(parsed: ParsedUdf) -> syn::Result<UdfSpec> {
    let ParsedUdf {
        function,
        options,
        context,
        arguments,
        gating,
    } = parsed;

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
    if function
        .sig
        .inputs
        .iter()
        .any(|input| matches!(input, FnArg::Receiver(_)))
    {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "Excel functions must be free functions",
        ));
    }

    let execution = analyze_execution(&function, &options, context.as_ref())?;
    let function_ident = function.sig.ident.clone();
    let id = UdfId::parse(
        options
            .id
            .clone()
            .unwrap_or_else(|| function_ident.unraw().to_string()),
        &function_ident,
    )?;
    let excel_name = options
        .name
        .clone()
        .unwrap_or_else(|| function_ident.unraw().to_string());
    xlfn_common::validate_function_name(&excel_name).map_err(|error| {
        syn::Error::new_spanned(
            &function_ident,
            format!("invalid Excel function name: {error}"),
        )
    })?;

    let return_type = match &function.sig.output {
        syn::ReturnType::Default => syn::parse_quote!(()),
        syn::ReturnType::Type(_, ty) => ty.as_ref().clone(),
    };
    let crate_path = resolve_crate_path(options.krate.as_ref());
    let return_abi = analyze_return_abi(
        &return_type,
        options.return_abi.as_deref(),
        &execution,
        &crate_path,
    )?;
    let strict_inputs = match options.input_semantics.as_deref() {
        None => false,
        Some("strict") => true,
        Some(_) => {
            return Err(syn::Error::new_spanned(
                &function.sig,
                "`input_semantics` must be `strict`",
            ));
        }
    };
    let handle_reuse = match options.handle_reuse.as_deref() {
        None => None,
        Some("inputs") => Some(true),
        Some("never") => Some(false),
        Some(_) => {
            return Err(syn::Error::new_spanned(
                &function.sig,
                "`handle_reuse` must be `inputs` or `never`",
            ));
        }
    };
    if handle_reuse.is_some()
        && (execution.is_async() || return_abi != xlfn_common::ReturnAbi::Value12)
    {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "`handle_reuse` requires a synchronous handle return using XLOPER12",
        ));
    }
    let arguments = analyze_arguments(arguments, &execution, &function, strict_inputs)?;
    let metadata = UdfMetadata {
        id,
        excel_name,
        category: options.category.clone().unwrap_or_default(),
        description: options
            .description
            .clone()
            .unwrap_or_else(|| doc_comment(&function.attrs)),
        help_topic: options.help_topic.clone().unwrap_or_default(),
    };
    for (field, value) in [
        ("Excel function category", metadata.category.as_str()),
        ("Excel function description", metadata.description.as_str()),
        ("Excel function help topic", metadata.help_topic.as_str()),
    ] {
        validate_registration_string(field, value, &function_ident)?;
    }

    Ok(UdfSpec {
        function,
        metadata,
        execution,
        arguments,
        return_type,
        return_abi,
        handle_reuse,
        volatile: options.volatile,
        hidden: options.hidden,
        gating,
        crate_path,
    })
}

fn analyze_return_abi(
    ty: &Type,
    requested: Option<&str>,
    execution: &ExecutionSpec,
    crate_path: &Path,
) -> syn::Result<xlfn_common::ReturnAbi> {
    use xlfn_common::ReturnAbi;
    match requested {
        Some("fp12") if execution.is_async() => Err(syn::Error::new_spanned(
            ty,
            "`return_abi = \"fp12\"` is only supported for synchronous Excel functions",
        )),
        // Rust's NumericReturn bound validates the resolved type, including
        // numeric aliases that syntax alone cannot recognize.
        Some("fp12") => Ok(ReturnAbi::NumericArray12),
        Some("xloper12") => Ok(ReturnAbi::Value12),
        Some(_) => Err(syn::Error::new_spanned(
            ty,
            "`return_abi` must be \"xloper12\" or \"fp12\"",
        )),
        None if execution.is_async() => Ok(ReturnAbi::Value12),
        None if is_numeric_array_return(ty, crate_path) => Ok(ReturnAbi::NumericArray12),
        None => Ok(ReturnAbi::Value12),
    }
}

/// Ordinary containers and aliases keep Q semantics. Only the dedicated
/// numeric output type opts into FP12, including when wrapped in Result.
fn is_numeric_array_return(ty: &Type, crate_path: &Path) -> bool {
    match ty {
        Type::Paren(paren) => return is_numeric_array_return(&paren.elem, crate_path),
        Type::Group(group) => return is_numeric_array_return(&group.elem, crate_path),
        _ => {}
    }
    let Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    let name = segment.ident.to_string();
    let prefix = path
        .path
        .segments
        .iter()
        .take(path.path.segments.len() - 1)
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    let framework = crate_path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    let framework_path = |modules: &[&str]| {
        prefix.is_empty()
            || modules.iter().any(|module| {
                prefix
                    == if module.is_empty() {
                        framework.clone()
                    } else {
                        format!("{framework}::{module}")
                    }
            })
    };
    let arguments = match &segment.arguments {
        PathArguments::None => None,
        PathArguments::AngleBracketed(arguments) => Some(&arguments.args),
        _ => return false,
    };
    match name.as_str() {
        "NumericArrayOutput" if framework_path(&["output"]) => arguments.is_none(),
        "XllResult" if framework_path(&["", "error", "prelude"]) => {
            let Some(arguments) = arguments else {
                return false;
            };
            arguments.len() == 1
                && matches!(arguments.first(), Some(GenericArgument::Type(inner))
                    if is_numeric_array_return(inner, crate_path))
        }
        "Result" if matches!(prefix.as_str(), "" | "std::result" | "core::result") => {
            let Some(arguments) = arguments else {
                return false;
            };
            arguments.len() == 2
                && matches!(arguments.first(), Some(GenericArgument::Type(inner))
                    if is_numeric_array_return(inner, crate_path))
        }
        _ => false,
    }
}

fn analyze_execution(
    function: &UdfFunction,
    options: &ParsedFunctionOptions,
    context: Option<&ParsedContext>,
) -> syn::Result<ExecutionSpec> {
    let is_async = function.sig.asyncness.is_some();
    let context_kind = context.map(|context| context.kind);
    if context_kind.is_some() && (options.thread_safe || options.macro_sheet) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "thread_safe and macro_sheet cannot be combined with an injected context",
        ));
    }
    if is_async && context_kind.is_some() && !matches!(context_kind, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an async Excel function can only inject AsyncContext",
        ));
    }
    if !is_async && matches!(context_kind, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "AsyncContext can only be injected by an async Excel function",
        ));
    }
    let quote = char::from(96);
    if options.macro_sheet && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig,
            format!("a macro-sheet function cannot be marked {quote}thread_safe{quote}"),
        ));
    }
    if options.macro_sheet && is_async {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "an async Excel function cannot be a macro-sheet function",
        ));
    }
    if options.thread_safe && is_async {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "an async Excel function cannot be marked thread_safe",
        ));
    }
    let context_ty = context.map(|context| context.ty.clone());
    Ok(if is_async {
        ExecutionSpec::Async { context_ty }
    } else if options.macro_sheet || matches!(context_kind, Some(ContextKind::MacroSheet)) {
        ExecutionSpec::MacroSheet { context_ty }
    } else if options.thread_safe || matches!(context_kind, Some(ContextKind::ThreadSafe)) {
        ExecutionSpec::ThreadSafe { context_ty }
    } else {
        ExecutionSpec::MainThread { context_ty }
    })
}

fn analyze_arguments(
    arguments: Vec<ParsedArgument>,
    execution: &ExecutionSpec,
    function: &UdfFunction,
    strict_inputs: bool,
) -> syn::Result<Vec<ArgumentSpec>> {
    let maximum_visible = xlfn_common::max_excel_function_arguments(execution.kind());
    if arguments.len() > maximum_visible {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!("Excel functions support at most {maximum_visible} arguments"),
        ));
    }

    let mut analyzed = Vec::with_capacity(arguments.len());
    for argument in arguments {
        if execution.is_async() && is_borrowed_string(&argument.ty) {
            return Err(syn::Error::new_spanned(
                &argument.ty,
                "&str borrows the current Excel call and is only supported by synchronous functions; use String for async arguments",
            ));
        }
        let excel_name = argument
            .options
            .name
            .clone()
            .unwrap_or_else(|| argument.rust_name.unraw().to_string());
        if xlfn_common::validate_argument_name(&excel_name).is_err() {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "Excel argument names must be non-empty counted strings without comma, NUL, CR, or LF",
            ));
        }

        let conversion = if parse_numeric_matrix_type(&argument.ty)? {
            if execution.is_async() {
                return Err(syn::Error::new_spanned(
                    &argument.ty,
                    "NumericMatrixRef borrows Excel's numeric array and is only supported by synchronous functions",
                ));
            }
            if argument.options.reference
                || argument.options.strict
                || argument.options.default.is_some()
                || argument.options.blank.is_some()
                || argument.options.missing.is_some()
            {
                return Err(syn::Error::new_spanned(
                    &function.sig.inputs,
                    "NumericMatrixRef uses Excel's numeric coercion and cannot use reference, strict, blank, missing, or default policies",
                ));
            }
            ArgumentConversion::NumericMatrix
        } else if let Some(object) = parse_handle_lease_type(&argument.ty)? {
            if !execution.is_async() {
                return Err(syn::Error::new_spanned(
                    &argument.ty,
                    "HandleLease arguments are only supported by async Excel functions",
                ));
            }
            if argument.options.reference
                || argument.options.strict
                || argument.options.default.is_some()
                || argument.options.blank.is_some()
                || argument.options.missing.is_some()
            {
                return Err(syn::Error::new_spanned(
                    &function.sig.inputs,
                    "HandleLease arguments cannot use reference, strict, blank, missing, or default policies",
                ));
            }
            ArgumentConversion::HandleLease {
                object: Box::new(object),
            }
        } else if argument.options.reference {
            if argument.options.strict
                || argument.options.default.is_some()
                || argument.options.blank.is_some()
                || argument.options.missing.is_some()
            {
                return Err(syn::Error::new_spanned(
                    &function.sig.inputs,
                    "reference arguments cannot use strict, blank, missing, or default policies",
                ));
            }
            ArgumentConversion::Reference
        } else {
            let blank = analyze_presence(
                "blank",
                argument.options.blank.as_deref(),
                argument.options.default.as_ref(),
                &function.sig.inputs,
            )?;
            let missing = analyze_presence(
                "missing",
                argument
                    .options
                    .missing
                    .as_deref()
                    .or_else(|| argument.options.default.as_ref().map(|_| "default")),
                argument.options.default.as_ref(),
                &function.sig.inputs,
            )?;
            let quote = char::from(96);
            let double_quote = char::from(34);
            if argument.options.default.is_some()
                && !matches!(blank, PresenceAction::Default(_))
                && !matches!(missing, PresenceAction::Default(_))
            {
                return Err(syn::Error::new_spanned(
                    &function.sig.inputs,
                    format!(
                        "{quote}default = ...{quote} requires {quote}blank = {quoted_default}{quote} or {quote}missing = {quoted_default}{quote}",
                        quoted_default = format!("{double_quote}default{double_quote}"),
                    ),
                ));
            }
            ArgumentConversion::Value(Box::new(ValueConversion { blank, missing }))
        };

        let abi = match &conversion {
            ArgumentConversion::Reference => xlfn_common::ArgumentAbi::Reference12,
            ArgumentConversion::NumericMatrix => xlfn_common::ArgumentAbi::NumericArray12,
            ArgumentConversion::HandleLease { .. } => xlfn_common::ArgumentAbi::Value12,
            ArgumentConversion::Value(_) => {
                if strict_inputs
                    || argument.options.strict
                    || argument.options.default.is_some()
                    || argument.options.blank.is_some()
                    || argument.options.missing.is_some()
                {
                    xlfn_common::ArgumentAbi::Value12
                } else {
                    native_argument_abi(&argument.ty)
                }
            }
        };
        let description = argument.options.description.unwrap_or_default();
        validate_registration_string(
            "Excel argument description",
            &description,
            &function.sig.inputs,
        )?;
        analyzed.push(ArgumentSpec {
            ty: argument.ty,
            excel_name,
            description,
            abi,
            conversion,
        });
    }

    let has_reference = analyzed
        .iter()
        .any(|argument| argument.conversion.is_reference());
    if has_reference && execution.is_async() {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an async Excel function cannot accept reference arguments",
        ));
    }
    if has_reference && !execution.is_macro_sheet() {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "a reference argument requires `macro_sheet` or MacroSheetContext",
        ));
    }

    let argument_names = analyzed
        .iter()
        .map(|argument| argument.excel_name.as_str())
        .collect::<Vec<_>>();
    if xlfn_common::validate_argument_names(&argument_names).is_err() {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "combined Excel argument names exceed the 32,767 UTF-16 unit counted-string limit",
        ));
    }

    Ok(analyzed)
}

/// Only recognized standard Rust spellings select a native ABI. In particular,
/// aliases and user-defined qualified types keep their FromExcel contract.
fn native_argument_abi(ty: &Type) -> xlfn_common::ArgumentAbi {
    use xlfn_common::ArgumentAbi;
    match ty {
        Type::Paren(paren) => native_argument_abi(&paren.elem),
        Type::Group(group) => native_argument_abi(&group.elem),
        Type::Reference(reference) if reference.mutability.is_none() => {
            if is_standard_path(
                &reference.elem,
                "str",
                &["core::primitive", "std::primitive"],
            ) {
                ArgumentAbi::CountedUtf16
            } else {
                ArgumentAbi::Value12
            }
        }
        _ if is_standard_path(ty, "f64", &["core::primitive", "std::primitive"]) => {
            ArgumentAbi::Float64
        }
        _ if is_standard_path(ty, "bool", &["core::primitive", "std::primitive"]) => {
            ArgumentAbi::Boolean16
        }
        _ if is_standard_path(ty, "i32", &["core::primitive", "std::primitive"]) => {
            ArgumentAbi::Integer32
        }
        _ if is_standard_path(ty, "String", &["std::string", "alloc::string"]) => {
            ArgumentAbi::CountedUtf16
        }
        _ => ArgumentAbi::Value12,
    }
}

fn is_standard_path(ty: &Type, name: &str, prefixes: &[&str]) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some()
        || path
            .path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, PathArguments::None))
    {
        return false;
    }
    let spelling = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    spelling == name
        || prefixes
            .iter()
            .any(|prefix| spelling == format!("{prefix}::{name}"))
}

pub(super) fn is_borrowed_string(ty: &Type) -> bool {
    match ty {
        Type::Paren(paren) => is_borrowed_string(&paren.elem),
        Type::Group(group) => is_borrowed_string(&group.elem),
        Type::Reference(reference) => {
            reference.mutability.is_none()
                && is_standard_path(
                    &reference.elem,
                    "str",
                    &["core::primitive", "std::primitive"],
                )
        }
        _ => false,
    }
}

/// Recognizes the framework's two-parameter generation-scoped handle lease
/// syntax without making the proc macro depend on the user's import path.
fn parse_handle_lease_type(ty: &Type) -> syn::Result<Option<Type>> {
    let Some(path) = context_type_path(ty) else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "HandleLease" {
        return Ok(None);
    }
    if !is_context_value_path(ty) {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease must be passed by value",
        ));
    }
    if path.qself.is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease cannot use qualified associated type paths",
        ));
    }

    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease must specify a lifetime and payload type",
        ));
    };
    if arguments.args.len() != 2 {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease must have the form HandleLease<'generation, T>",
        ));
    }
    let mut arguments = arguments.args.iter();
    let Some(GenericArgument::Lifetime(lifetime)) = arguments.next() else {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease must specify its generation lifetime first",
        ));
    };
    if lifetime.ident == "static" {
        return Err(syn::Error::new_spanned(
            lifetime,
            "HandleLease cannot use the 'static lifetime",
        ));
    }
    let Some(GenericArgument::Type(object)) = arguments.next() else {
        return Err(syn::Error::new_spanned(
            ty,
            "HandleLease must specify an object payload type",
        ));
    };
    Ok(Some(object.clone()))
}

/// Numeric arrays borrow the current Excel call. Rust permits the lifetime
/// to be elided, but no other generic arguments or owning wrappers apply.
fn parse_numeric_matrix_type(ty: &Type) -> syn::Result<bool> {
    let Some(path) = context_type_path(ty) else {
        return Ok(false);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(false);
    };
    if segment.ident != "NumericMatrixRef" {
        return Ok(false);
    }
    if !is_context_value_path(ty) {
        return Err(syn::Error::new_spanned(
            ty,
            "NumericMatrixRef must be passed by value",
        ));
    }
    if path.qself.is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "NumericMatrixRef cannot use qualified associated type paths",
        ));
    }
    match &segment.arguments {
        PathArguments::None => Ok(true),
        PathArguments::AngleBracketed(arguments) if arguments.args.len() == 1 => {
            if let Some(GenericArgument::Lifetime(lifetime)) = arguments.args.first() {
                if lifetime.ident == "static" {
                    return Err(syn::Error::new_spanned(
                        lifetime,
                        "NumericMatrixRef cannot use the 'static lifetime",
                    ));
                }
                return Ok(true);
            }
            Err(syn::Error::new_spanned(
                ty,
                "NumericMatrixRef accepts only its call lifetime",
            ))
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "NumericMatrixRef accepts only its call lifetime",
        )),
    }
}

fn analyze_presence(
    name: &str,
    policy: Option<&str>,
    default: Option<&Expr>,
    span: &impl ToTokens,
) -> syn::Result<PresenceAction> {
    let quote = char::from(96);
    let double_quote = char::from(34);
    let quoted_default = format!("{double_quote}default{double_quote}");
    match policy {
        None | Some("convert") => Ok(PresenceAction::Convert),
        Some("default") => Ok(PresenceAction::Default(default.cloned().ok_or_else(|| {
            syn::Error::new_spanned(
                span,
                format!(
                    "{quote}{name} = {quoted_default}{quote} requires {quote}default = ...{quote}"
                ),
            )
        })?)),
        Some("error") => Ok(PresenceAction::Error),
        Some(_) => Err(syn::Error::new_spanned(
            span,
            format!(
                "{quote}{name}{quote} must be {quoted_default}, {quote}error{quote}, or {quote}convert{quote}"
            ),
        )),
    }
}

/// Lower validated semantics into the names and indexes consumed by the
/// emitter. This pass cannot fail because all user-facing validation is done
/// by analyze.
pub(super) fn lower(spec: UdfSpec) -> UdfPlan {
    let udf_id = spec.metadata.id.as_str();
    let symbols = UdfSymbols {
        export_ident: format_ident!("xll_{udf_id}"),
        descriptor_ident: format_ident!("__XLFN_DESCRIPTOR_{udf_id}"),
        manifest_ident: format_ident!("__XLFN_EXP_{udf_id}"),
        export_name_bytes: format!("xll_{udf_id}\0").into_bytes(),
    };
    let arguments = spec
        .arguments
        .into_iter()
        .enumerate()
        .map(|(index, argument)| ArgumentPlan {
            index,
            ty: argument.ty,
            excel_name: argument.excel_name,
            description: argument.description,
            raw_ident: format_ident!("__raw_{index}"),
            local_ident: format_ident!("__argument_{index}"),
            abi: argument.abi,
            conversion: argument.conversion,
        })
        .collect();

    UdfPlan {
        function: spec.function,
        metadata: spec.metadata,
        execution: spec.execution,
        arguments,
        return_type: spec.return_type,
        return_abi: spec.return_abi,
        handle_reuse: spec.handle_reuse,
        volatile: spec.volatile,
        hidden: spec.hidden,
        gating: spec.gating,
        crate_path: spec.crate_path,
        symbols,
    }
}
