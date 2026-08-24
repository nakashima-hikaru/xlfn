//! The UDF front end: parsed syntax, semantic analysis, and lowering.
//!
//! This module intentionally contains no generated TokenStream. Raw attribute
//! values are accepted by the parser, normalized into semantic enums by the
//! analyzer, and only then lowered into names needed by the emitter.

use crate::options::{
    ContextKind, ParsedArgumentOptions, ParsedFunctionOptions, parse_argument_options,
    parse_context_attribute, parse_function_options,
};
use crate::support::{doc_comment, extract_gating_attributes, resolve_crate_path};
use crate::validation::validate_export_id;
use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident};
use syn::{Attribute, Expr, FnArg, Ident, ItemFn, Pat, Path, Type};

/// Syntax extracted from one excel_function item.
pub(super) struct ParsedUdf {
    pub(super) function: ItemFn,
    pub(super) options: ParsedFunctionOptions,
    pub(super) context: Option<ParsedContext>,
    pub(super) arguments: Vec<ParsedArgument>,
    pub(super) gating: Vec<Attribute>,
}

pub(super) struct ParsedContext {
    pub(super) kind: ContextKind,
    pub(super) ty: Type,
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
/// presence actions or a raw reference conversion.
pub(super) enum ArgumentConversion {
    Value(Box<ValueConversion>),
    Reference,
}

pub(super) struct ValueConversion {
    pub(super) blank: PresenceAction,
    pub(super) missing: PresenceAction,
}

impl ArgumentConversion {
    pub(super) const fn is_reference(&self) -> bool {
        matches!(self, Self::Reference)
    }

    pub(super) const fn requires_presence_check(&self) -> bool {
        match self {
            Self::Reference => false,
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
    pub(super) function: ItemFn,
    pub(super) metadata: UdfMetadata,
    pub(super) execution: ExecutionSpec,
    pub(super) arguments: Vec<ArgumentSpec>,
    pub(super) return_type: Type,
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
    pub(super) conversion: ArgumentConversion,
}

/// Code-generation-oriented IR. It contains generated symbols, but still no
/// emitted tokens or semantic policy interpretation.
pub(super) struct UdfPlan {
    pub(super) function: ItemFn,
    pub(super) metadata: UdfMetadata,
    pub(super) execution: ExecutionSpec,
    pub(super) arguments: Vec<ArgumentPlan>,
    pub(super) return_type: Type,
    pub(super) volatile: bool,
    pub(super) hidden: bool,
    pub(super) gating: Vec<Attribute>,
    pub(super) crate_path: Path,
    pub(super) symbols: UdfSymbols,
}

/// Parse only syntax and remove macro-owned argument attributes from the item.
pub(super) fn parse_udf(attributes: TokenStream, mut function: ItemFn) -> syn::Result<ParsedUdf> {
    let options = parse_function_options(attributes)?;
    let gating = extract_gating_attributes(&function.attrs);
    let mut context = None;
    let mut arguments = Vec::new();

    for (index, input) in function.sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = input else {
            continue;
        };

        let mut retained = Vec::new();
        let mut argument_context = None;
        let mut has_excel_arg = false;
        let mut excel_arg_attribute = None;
        let mut argument_options = ParsedArgumentOptions::default();
        for attribute in std::mem::take(&mut argument.attrs) {
            if attribute.path().is_ident("excel_context") {
                let kind = parse_context_attribute(&attribute)?;
                if argument_context.replace(kind).is_some() {
                    return Err(syn::Error::new_spanned(
                        attribute,
                        "an argument can have only one #[excel_context(...)] role",
                    ));
                }
            } else if attribute.path().is_ident("excel_arg") {
                has_excel_arg = true;
                parse_argument_options(&attribute, &mut argument_options)?;
                excel_arg_attribute = Some(attribute);
            } else {
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
                if let Some(attribute) = excel_arg_attribute {
                    argument.attrs.push(attribute);
                }
                return Err(syn::Error::new_spanned(
                    argument,
                    "#[excel_context(...)] cannot be combined with #[excel_arg(...)]",
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
            .unwrap_or_else(|| function_ident.to_string()),
        &function_ident,
    )?;
    let excel_name = options
        .name
        .clone()
        .unwrap_or_else(|| function_ident.to_string());
    if excel_name.trim().is_empty() {
        return Err(syn::Error::new_spanned(
            &function_ident,
            "Excel function name cannot be empty",
        ));
    }

    let return_type = match &function.sig.output {
        syn::ReturnType::Default => syn::parse_quote!(()),
        syn::ReturnType::Type(_, ty) => ty.as_ref().clone(),
    };
    let arguments = analyze_arguments(arguments, &execution, &function)?;
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

    Ok(UdfSpec {
        function,
        metadata,
        execution,
        arguments,
        return_type,
        volatile: options.volatile,
        hidden: options.hidden,
        gating,
        crate_path: resolve_crate_path(options.krate.as_ref()),
    })
}

fn analyze_execution(
    function: &ItemFn,
    options: &ParsedFunctionOptions,
    context: Option<&ParsedContext>,
) -> syn::Result<ExecutionSpec> {
    let is_async = function.sig.asyncness.is_some();
    let context_kind = context.map(|context| context.kind);
    if is_async && context_kind.is_some() && !matches!(context_kind, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an async Excel function must use #[excel_context(asynchronous)]",
        ));
    }
    if !is_async && matches!(context_kind, Some(ContextKind::Async)) {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "#[excel_context(asynchronous)] can only be used by an async Excel function",
        ));
    }
    let quote = char::from(96);
    if matches!(context_kind, Some(ContextKind::MainThread)) && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!("a main-thread context function cannot be marked {quote}thread_safe{quote}"),
        ));
    }
    if matches!(context_kind, Some(ContextKind::ThreadSafe)) && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!(
                "a thread-safe context function must not repeat the {quote}thread_safe{quote} mode flag"
            ),
        ));
    }
    if matches!(context_kind, Some(ContextKind::MacroSheet)) && options.thread_safe {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!("a macro-sheet context function cannot be marked {quote}thread_safe{quote}"),
        ));
    }
    if matches!(context_kind, Some(ContextKind::MacroSheet)) && options.macro_sheet {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!(
                "a macro-sheet context function must not repeat the {quote}macro_sheet{quote} mode flag"
            ),
        ));
    }
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
    if options.macro_sheet
        && matches!(
            context_kind,
            Some(ContextKind::MainThread | ContextKind::ThreadSafe | ContextKind::Async)
        )
    {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "`macro_sheet` is incompatible with this context role",
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
    function: &ItemFn,
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
        let excel_name = argument
            .options
            .name
            .clone()
            .unwrap_or_else(|| argument.rust_name.to_string());
        if xlfn_common::validate_argument_name(&excel_name).is_err() {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "Excel argument names must be non-empty counted strings without comma, NUL, CR, or LF",
            ));
        }

        let conversion = if argument.options.reference {
            if argument.options.default.is_some()
                || argument.options.blank.is_some()
                || argument.options.missing.is_some()
            {
                return Err(syn::Error::new_spanned(
                    &function.sig.inputs,
                    "reference arguments cannot use blank, missing, or default policies",
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
                argument.options.missing.as_deref(),
                argument.options.default.as_ref(),
                &function.sig.inputs,
            )?;
            let quote = char::from(96);
            let double_quote = char::from(34);
            if argument.options.default.is_some()
                && argument.options.blank.as_deref() != Some("default")
                && argument.options.missing.as_deref() != Some("default")
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

        analyzed.push(ArgumentSpec {
            ty: argument.ty,
            excel_name,
            description: argument.options.description.unwrap_or_default(),
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
        None => Ok(PresenceAction::Convert),
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
                "{quote}{name}{quote} must be {quoted_default} or {quote}error{quote}"
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
            conversion: argument.conversion,
        })
        .collect();

    UdfPlan {
        function: spec.function,
        metadata: spec.metadata,
        execution: spec.execution,
        arguments,
        return_type: spec.return_type,
        volatile: spec.volatile,
        hidden: spec.hidden,
        gating: spec.gating,
        crate_path: spec.crate_path,
        symbols,
    }
}
