use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(thread_safe)]
fn main_thread_as_thread(context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(macro_sheet)]
fn main_thread_as_macro(context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(thread_safe)]
fn redundant_thread(context: ThreadSafeContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(macro_sheet)]
fn thread_as_macro(context: ThreadSafeContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(thread_safe)]
fn macro_as_thread(context: MacroSheetContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(macro_sheet)]
fn redundant_macro(context: MacroSheetContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
