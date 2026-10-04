use xlfn::prelude::*;

#[excel_addin(name = "Context Modes", id = "context-modes", category = "Test")]
pub struct TestAddin;

impl Addin for TestAddin {
    type SharedState = i32;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(17))
    }
}

#[excel_function(name = "TEST.THREAD", thread_safe)]
fn thread_safe(#[excel_context(thread_safe)] context: ThreadSafeContext<'_, TestAddin>) -> i32 {
    *context.state()
}

fn main_state<'call>(context: MainThreadContext<'call, TestAddin>) -> &'call i32 {
    context.state()
}

fn macro_state<'call>(context: MacroSheetContext<'call, TestAddin>) -> &'call i32 {
    context.state()
}

#[excel_function(name = "TEST.MAIN")]
fn main_thread(#[excel_context(main_thread)] context: MainThreadContext<'_, TestAddin>) -> i32 {
    *main_state(context)
}

#[excel_function(name = "TEST.MACRO", macro_sheet)]
fn macro_sheet(#[excel_context(macro_sheet)] context: MacroSheetContext<'_, TestAddin>) -> i32 {
    *macro_state(context)
}

fn main() {}
