use xlfn::prelude::*;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

mod custom {
    use std::marker::PhantomData;
    use xlfn::XllResult;
    use xlfn::value::{FromExcel, XlValueRef};

    pub struct AsyncContext<'call, A>(PhantomData<&'call A>);

    impl<'call, A> FromExcel<'call> for AsyncContext<'call, A> {
        fn from_excel(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
            Ok(Self(PhantomData))
        }
    }
}

#[excel_function]
async fn bad(context: custom::AsyncContext<'_, TestAddin>) -> f64 {
    let _ = context;
    std::future::ready(()).await;
    0.0
}

fn main() {}
