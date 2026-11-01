// module: macro_abi
use crate::__private::v1::*;
fn threadsafe<'call, A: crate::Addin>(
    environment: &CallEnvironment<'call, (), crate::DynamicRtdSources<()>, ThreadSafeMode>,
    state: &'call A::SharedState,
) { let _ = thread_safe_context::<A, _, _>(environment, state); }
fn macro_sheet<'call, A: crate::Addin>(
    environment: &CallEnvironment<'call, (), crate::DynamicRtdSources<()>, MacroSheetMode>,
    state: &'call A::SharedState,
) { let _ = macro_sheet_context::<A, _, _>(environment, state); }
