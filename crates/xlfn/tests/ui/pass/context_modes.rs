use xlfn::prelude::*;
use xlfn::{MacroSheetContext, MainThreadContext, ThreadSafeContext};

#[excel_addin(name = "Context Modes", id = "context-modes", category = "Test")]
pub struct TestAddin;

impl Addin for TestAddin {
    type SharedState = i32;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(17))
    }
}

#[excel_function(name = "TEST.THREAD")]
fn thread_safe(context: ThreadSafeContext<'_, TestAddin>) -> i32 {
    *context.state()
}

fn main_state<'call>(context: MainThreadContext<'call, TestAddin>) -> &'call i32 {
    context.state()
}

fn macro_state<'call>(context: MacroSheetContext<'call, TestAddin>) -> &'call i32 {
    context.state()
}

#[excel_function(name = "TEST.MAIN")]
fn main_thread(context: MainThreadContext<'_, TestAddin>) -> i32 {
    *main_state(context)
}

#[excel_function(name = "TEST.MACRO")]
fn macro_sheet(context: MacroSheetContext<'_, TestAddin>) -> i32 {
    *macro_state(context)
}

#[excel_function(name = "TEST.QUALIFIED.MAIN")]
fn qualified_main(context: xlfn::MainThreadContext<'_, TestAddin>, value: f64) -> f64 {
    value + f64::from(*context.state())
}

#[excel_function(name = "TEST.QUALIFIED.THREAD")]
fn qualified_thread(context: xlfn::ThreadSafeContext<'_, TestAddin>) -> i32 {
    *context.state()
}

#[excel_function(name = "TEST.QUALIFIED.MACRO")]
fn qualified_macro(context: xlfn::MacroSheetContext<'_, TestAddin>) -> i32 {
    *context.state()
}

#[excel_function(name = "TEST.NO.CONTEXT.DEFAULT")]
fn no_context_default(value: f64) -> f64 {
    value
}

#[excel_function(name = "TEST.NO.CONTEXT.THREAD", thread_safe)]
fn no_context_thread(value: f64) -> f64 {
    value
}

#[excel_function(name = "TEST.NO.CONTEXT.MACRO", macro_sheet)]
fn no_context_macro(value: f64) -> f64 {
    value
}

#[excel_function(name = "TEST.ELIDED.THREAD")]
fn elided_thread(context: ThreadSafeContext<TestAddin>, value: f64) -> f64 {
    value + f64::from(*context.state())
}

fn main() {
    type ValueExport = unsafe extern "system" fn() -> *mut xlfn::__private::v1::XLOPER12;
    type OneArgumentExport = unsafe extern "system" fn(
        *mut xlfn::__private::v1::XLOPER12,
    ) -> *mut xlfn::__private::v1::XLOPER12;

    // Injected capabilities do not add worksheet-visible ABI arguments.
    let _: ValueExport = xll_thread_safe;
    let _: ValueExport = xll_main_thread;
    let _: ValueExport = xll_macro_sheet;
    let _: OneArgumentExport = xll_qualified_main;
    let _: OneArgumentExport = xll_elided_thread;
}
