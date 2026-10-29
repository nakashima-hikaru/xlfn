// Compile-fixture executor; dropping admitted tasks completes their ownership.
pub struct Executor;
impl xlfn::AsyncExecutor for Executor {
    type Reservation = ();
    fn start(&self) -> xlfn::XllResult<()> { Ok(()) }
    fn reserve(&self) -> xlfn::XllResult<()> { Ok(()) }
    fn submit<F>(&self, (): (), task: xlfn::AsyncTask<F>)
    where F: std::future::Future<Output = ()> + Send + 'static {
        drop(task);
    }
    fn shutdown(&self) -> xlfn::XllResult<()> { Ok(()) }
}
