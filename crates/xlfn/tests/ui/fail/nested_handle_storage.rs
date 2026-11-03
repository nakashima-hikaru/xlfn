use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Inner;

#[derive(ExcelHandleObject)]
struct Nested<'call> {
    inner: Handle<'call, Inner>,
}

fn require_handle_object<T: xlfn::handle::ExcelHandleObject>() {}

fn main() {
    require_handle_object::<Nested<'static>>();
}
