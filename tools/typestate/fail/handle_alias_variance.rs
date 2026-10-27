// module: handle
use super::typed::{ExcelHandleObject, HandleAlias};

struct Payload<F>(F);
impl<F: Send + Sync + 'static> ExcelHandleObject for Payload<F> {}

fn invalid<'call>(
    alias: HandleAlias<'call, Payload<for<'a> fn(&'a str)>>,
) -> HandleAlias<'call, Payload<fn(&'static str)>> {
    alias // error: E0308
}
