//! Semantic models consumed by macro code generation.

use crate::options::{ContextKind, FunctionOptions};
use syn::ItemFn;

/// The validated execution facts needed by the UDF code generator.
///
/// Parsing attributes produces syntax-oriented values. This model is the
/// semantic boundary: incompatible context, async, and registration modes
/// are rejected before any ABI tokens are emitted.
pub(super) struct FunctionModel {
    pub(super) is_async: bool,
    pub(super) context: Option<ContextKind>,
    pub(super) macro_sheet: bool,
    pub(super) thread_safe: bool,
}

impl FunctionModel {
    pub(super) fn new(
        function: &ItemFn,
        options: &FunctionOptions,
        context: Option<ContextKind>,
    ) -> Self {
        Self {
            is_async: function.sig.asyncness.is_some(),
            context,
            macro_sheet: options.macro_sheet,
            thread_safe: options.thread_safe,
        }
    }

    pub(super) fn validate(&self, function: &ItemFn) -> syn::Result<()> {
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
        Ok(())
    }

    pub(super) fn effective_macro_sheet(&self) -> bool {
        self.macro_sheet || matches!(self.context, Some(ContextKind::MacroSheet))
    }

    pub(super) fn effective_thread_safe(&self) -> bool {
        self.is_async || matches!(self.context, Some(ContextKind::ThreadSafe)) || self.thread_safe
    }
}
