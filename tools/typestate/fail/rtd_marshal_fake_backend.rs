// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, RtdUpdate, SubscriptionHost};
use super::publication::RefreshPublication;
use xlfn_host::refresh_array::ArrayApi;

struct FakeBackend;
// SAFETY: create always returns an error, so this backend never owns an array.
unsafe impl ArrayApi for FakeBackend {
    type Update = RtdUpdate;
    type Array = usize;
    type Data = ();
    fn create(_: u32) -> Result<usize, i32> { Err(-1) }
    unsafe fn access(_: usize) -> Result<(), i32> { unreachable!() }
    unsafe fn fill(_: (), _: &[RtdUpdate]) -> Result<(), i32> { unreachable!() }
    unsafe fn unaccess(_: usize) -> Result<(), i32> { unreachable!() }
    unsafe fn destroy(_: usize) { unreachable!() }
    fn retained(_: usize, _: i32) { unreachable!() }
}

fn invalid<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>) {
    let _ = RefreshPublication::<H, FakeBackend>::prepare(batch); // error: E0624
}
