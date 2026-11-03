use xlfn::value::{InputIdentityEncoder, PrepareExcel, XlValueRef};
use xlfn::XllResult;

fn escape<'call>(value: XlValueRef<'call>, identity: &mut InputIdentityEncoder)
    -> XllResult<<String as PrepareExcel<'static>>::Prepared>
{
    <String as PrepareExcel<'call>>::prepare(value, "value", identity)
}

fn main() {}
