// module: excel_rtd/windows/server
use std::marker::PhantomData;
struct Subscription<F: Send + 'static>(PhantomData<F>);
// SAFETY: this empty fixture owns no sink, callback, or worker.
#[allow(unsafe_code)]
unsafe impl<F: Send + 'static> crate::RtdSubscription for Subscription<F> {
    fn request_cancel(&self) {}
    fn disconnect_and_wait(self) -> crate::XllResult<()> { Ok(()) }
}
type AnyLifetime = Subscription<for<'a> fn(&'a ())>;
type StaticOnly = Subscription<fn(&'static ())>;
fn invalid(work: super::DeferredTerminationWork<AnyLifetime, ()>) -> super::DeferredTerminationWork<StaticOnly, ()> {
    work // error: E0308
}
