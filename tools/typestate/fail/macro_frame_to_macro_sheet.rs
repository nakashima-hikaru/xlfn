// module: macro_abi
use crate::__private::v1::*;

fn async_to_macro_sheet<'call, A: crate::Addin>(
    environment: &CallEnvironment<'call, (), crate::DynamicRtdSources<()>, AsyncLaunchMode>,
    state: &'call A::SharedState,
) {
    let _ = macro_sheet_context::<A, _, _>(environment, state); // error: E0308
}

fn threadsafe_to_macro_sheet<'call, A: crate::Addin>(
    environment: &CallEnvironment<'call, (), crate::DynamicRtdSources<()>, ThreadSafeMode>,
    state: &'call A::SharedState,
) {
    let _ = macro_sheet_context::<A, _, _>(environment, state); // error: E0308
}
