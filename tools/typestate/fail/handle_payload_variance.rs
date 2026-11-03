// module: handle
use super::typed::{ExcelHandleObject, Handle};

struct Payload<F>(F);
impl<F: Send + Sync + 'static> ExcelHandleObject for Payload<F> {}

fn invalid<'call>(
    handle: Handle<'call, Payload<for<'a> fn(&'a str)>>,
) -> Handle<'call, Payload<fn(&'static str)>> {
    handle // error: E0308
}
