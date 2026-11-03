// module: value
use crate::value::input::{ArgumentContext, FinishedArguments, PlainInputMode};
fn finish_digest<'call>(mut arguments: ArgumentContext<'_, 'call, PlainInputMode>) -> crate::XllResult<Option<[u8; 32]>> {
    arguments.native_f64(0, "number", 1.0)?;
    arguments.finish()
}
fn finish_typed<'call>(arguments: ArgumentContext<'_, 'call, PlainInputMode>) -> crate::XllResult<FinishedArguments<'call, PlainInputMode>> {
    arguments.finish_typed()
}
