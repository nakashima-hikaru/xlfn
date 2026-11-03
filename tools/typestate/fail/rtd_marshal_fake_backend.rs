// module: subscription/data_plane/refresh/windows
use super::RtdUpdate;
struct FakeBackend;
impl super::publication::ArrayApi for FakeBackend { // error: E0603
    type Array = usize;
    type Data = ();
    fn create(_: u32) -> Result<usize, i32> { Ok(1) }
    fn access(_: usize) -> Result<(), i32> { Ok(()) }
    fn fill(_: (), _: &[RtdUpdate]) -> Result<(), i32> { Ok(()) }
    fn unaccess(_: usize) -> Result<(), i32> { Ok(()) }
    fn destroy(_: usize) {}
    fn retained(_: usize, _: i32) {}
}
