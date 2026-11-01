// module: macro_abi
use crate::__private::v1::*;
fn finish_frame<'call>(
    frame: CallFrame<'call, crate::value::PlainInputMode>,
) -> crate::XllResult<()> {
    let (environment, finished) = frame.finish_arguments()?;
    let _ = (environment, finished);
    Ok(())
}
