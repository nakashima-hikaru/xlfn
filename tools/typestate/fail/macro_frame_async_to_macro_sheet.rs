// module: macro_abi
use crate::__private::v1::*;
fn invalid<'call, A: crate::Addin>(
    frame: &CallFrame<'call, crate::value::PlainInputMode, (), crate::DynamicRtdSources<()>, AsyncLaunchMode>,
    state: &'call A::SharedState,
) {
    let _ = macro_sheet_context::<A, _, _, _>(frame, state); // error: E0308
}
