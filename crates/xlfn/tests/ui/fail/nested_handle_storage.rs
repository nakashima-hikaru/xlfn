use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Inner;

#[derive(ExcelHandleObject)]
struct Nested<'call> {
    inner: Handle<'call, Inner>,
}

fn main() {}
