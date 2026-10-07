use xlfn::prelude::*;

#[excel_addin(name = "Scoped Context", id = "scoped-context", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[derive(ExcelHandleObject)]
struct Dataset {
    value: f64,
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
async fn bad(
    context: custom::AsyncContext<'_, TestAddin>,
    dataset: HandleLease<'_, Dataset>,
) -> f64 {
    let _ = context;
    std::future::ready(()).await;
    dataset.value
}

fn main() {}
