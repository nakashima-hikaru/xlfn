// module: value
use crate::value::input::{FinishedArguments, PlainInputMode};
fn finished_return_once<'call>(
    finished: FinishedArguments<'call, PlainInputMode>,
    first_access: crate::call_return::ReturnFrameAccess<'call>,
    second_access: crate::call_return::ReturnFrameAccess<'call>,
) {
    let _ = finished.return_context::<(), crate::DynamicRtdSources<()>>(first_access);
    let _ = finished.return_context::<(), crate::DynamicRtdSources<()>>(second_access); // error: E0382
}
