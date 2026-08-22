use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

fn assert_send_sync_static<T: Send + Sync + 'static>() {}

fn main() {
    xlfn::__private::assert_async_parameter::<PinnedHandle<Dataset>>();
    assert_send_sync_static::<PinnedHandle<Dataset>>();
}
