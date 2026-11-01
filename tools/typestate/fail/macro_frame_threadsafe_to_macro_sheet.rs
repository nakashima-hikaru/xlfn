// module: macro_abi
use crate::__private::v1::*;
fn invalid<'call, A: crate::Addin>(
    environment: &CallEnvironment<'call, (), crate::DynamicRtdSources<()>, ThreadSafeMode>,
    state: &'call A::SharedState,
) {
    let _ = macro_sheet_context::<A, _, _>(environment, state); // error: E0308
}
