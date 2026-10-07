use xlfn::prelude::*;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function(thread_safe)]
async fn context_as_thread(context: AsyncContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(macro_sheet)]
async fn context_as_macro(context: AsyncContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function(thread_safe)]
async fn no_context_thread(value: f64) -> f64 {
    value
}

#[excel_function(macro_sheet)]
async fn no_context_macro(value: f64) -> f64 {
    value
}

fn main() {}
