//! Semantic models consumed by macro code generation.

use crate::options::{ArgumentOptions, ContextKind, FunctionOptions, string_value};
use quote::format_ident;
use syn::punctuated::Punctuated;
use syn::{FnArg, Ident, ItemFn, Meta, Pat, Token, Type};

/// The single normalized execution mode consumed by code generation.
///
/// Attribute syntax may express the same mode in more than one way (for
/// example `macro_sheet` or `MacroSheetContext`).  Code generation must never
/// repeat that precedence logic, so the validated model exposes this enum as
/// its only execution-mode vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExecutionMode {
    MainThread,
    ThreadSafe,
    MacroSheet,
    Async,
}

impl ExecutionMode {
    pub(super) const fn is_async(self) -> bool {
        matches!(self, Self::Async)
    }

    pub(super) const fn is_macro_sheet(self) -> bool {
        matches!(self, Self::MacroSheet)
    }

    pub(super) const fn is_thread_safe(self) -> bool {
        matches!(self, Self::ThreadSafe | Self::Async)
    }
}

/// The validated execution facts needed by the UDF code generator.
///
/// Parsing attributes produces syntax-oriented values. This model is the
/// semantic boundary: incompatible context, async, and registration modes
/// are rejected before any ABI tokens are emitted.
pub(super) struct FunctionModel {
    pub(super) is_async: bool,
    pub(super) context: Option<ContextKind>,
    pub(super) context_type: Option<Type>,
    pub(super) macro_sheet: bool,
    pub(super) thread_safe: bool,
}

/// Fully validated semantic input to the code generator.
pub(super) struct ValidatedFunction {
    pub(super) mode: ExecutionMode,
    pub(super) context: Option<ContextKind>,
    pub(super) context_type: Option<Type>,
    pub(super) arguments: Vec<ArgumentModel>,
}

/// One visible Excel argument after syntax and policy validation.
pub(super) struct ArgumentModel {
    pub(super) ty: Type,
    pub(super) raw_name: Ident,
    pub(super) converted_name: Ident,
    pub(super) options: ArgumentOptions,
    pub(super) excel_name: String,
    pub(super) description: String,
}

impl FunctionModel {
    pub(super) fn new(
        function: &ItemFn,
        options: &FunctionOptions,
        context: Option<ContextKind>,
        context_type: Option<Type>,
    ) -> Self {
        Self {
            is_async: function.sig.asyncness.is_some(),
            context,
            context_type,
            macro_sheet: options.macro_sheet,
            thread_safe: options.thread_safe,
        }
    }

    pub(super) fn validate(self, function: &ItemFn) -> syn::Result<ValidatedFunction> {
        if self.is_async
            && self.context.is_some()
            && !matches!(self.context, Some(ContextKind::Async))
        {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "an async Excel function must use #[excel_context(asynchronous)]",
            ));
        }
        if !self.is_async && matches!(self.context, Some(ContextKind::Async)) {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "#[excel_context(asynchronous)] can only be used by an async Excel function",
            ));
        }
        if matches!(self.context, Some(ContextKind::MainThread)) && self.thread_safe {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "a main-thread context function cannot be marked `thread_safe`",
            ));
        }
        if matches!(self.context, Some(ContextKind::MacroSheet)) && self.thread_safe {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "a macro-sheet context function cannot be marked `thread_safe`",
            ));
        }
        if self.macro_sheet && self.thread_safe {
            return Err(syn::Error::new_spanned(
                &function.sig,
                "a macro-sheet function cannot be marked `thread_safe`",
            ));
        }
        if self.macro_sheet && self.is_async {
            return Err(syn::Error::new_spanned(
                &function.sig,
                "an async Excel function cannot be a macro-sheet function",
            ));
        }
        if self.macro_sheet
            && matches!(
                self.context,
                Some(ContextKind::MainThread | ContextKind::ThreadSafe | ContextKind::Async)
            )
        {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "`macro_sheet` is incompatible with this context role",
            ));
        }
        let mode = if self.is_async {
            ExecutionMode::Async
        } else if self.macro_sheet || matches!(self.context, Some(ContextKind::MacroSheet)) {
            ExecutionMode::MacroSheet
        } else if self.thread_safe || matches!(self.context, Some(ContextKind::ThreadSafe)) {
            ExecutionMode::ThreadSafe
        } else {
            ExecutionMode::MainThread
        };
        Ok(ValidatedFunction {
            mode,
            context: self.context,
            context_type: self.context_type,
            arguments: Vec::new(),
        })
    }
}

impl ValidatedFunction {
    pub(super) fn with_arguments(mut self, arguments: Vec<ArgumentModel>) -> Self {
        self.arguments = arguments;
        self
    }
}

pub(super) fn validate_arguments(
    function: &mut ItemFn,
    mode: ExecutionMode,
    context: Option<ContextKind>,
) -> syn::Result<Vec<ArgumentModel>> {
    let skip = usize::from(context.is_some());
    let mut arguments = Vec::new();

    for input in function.sig.inputs.iter_mut().skip(skip) {
        let FnArg::Typed(argument) = input else {
            return Err(syn::Error::new_spanned(
                input,
                "Excel functions must be free functions",
            ));
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

        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &argument.pat,
                "Excel function arguments must use simple identifier patterns",
            ));
        };
        arguments.push((pattern.ident.clone(), argument.ty.as_ref().clone(), options));
    }

    let maximum_visible = if mode.is_async() { 254 } else { 255 };
    if arguments.len() > maximum_visible {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            format!("Excel functions support at most {maximum_visible} arguments"),
        ));
    }

    let mut excel_arguments = Vec::with_capacity(arguments.len());
    for (index, (rust_name, ty, options)) in arguments.into_iter().enumerate() {
        let excel_name = options
            .name
            .clone()
            .unwrap_or_else(|| rust_name.to_string());
        let utf16_len = excel_name.encode_utf16().count();
        if excel_name.is_empty()
            || excel_name.contains([',', '\0', '\r', '\n'])
            || utf16_len > 32_767
        {
            return Err(syn::Error::new_spanned(
                &function.sig.inputs,
                "Excel argument names must be non-empty counted strings without comma, NUL, CR, or LF",
            ));
        }
        excel_arguments.push(ArgumentModel {
            ty,
            raw_name: format_ident!("__raw_{index}"),
            converted_name: format_ident!("__argument_{index}"),
            description: options.description.clone().unwrap_or_default(),
            options,
            excel_name,
        });
    }

    let joined_argument_name_len = excel_arguments
        .iter()
        .map(|argument| argument.excel_name.encode_utf16().count())
        .sum::<usize>()
        .saturating_add(excel_arguments.len().saturating_sub(1));
    if joined_argument_name_len > 32_767 {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "combined Excel argument names exceed the 32,767 UTF-16 unit counted-string limit",
        ));
    }

    Ok(excel_arguments)
}
