use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

mod custom {
    use std::marker::PhantomData;
    use xlfn::XllResult;
    use xlfn::value::{FromExcel, XlValueRef};

    pub struct MainThreadContext<'call, A>(PhantomData<&'call A>);
    pub struct ThreadSafeContext<'call, A>(PhantomData<&'call A>);
    pub struct MacroSheetContext<'call, A>(PhantomData<&'call A>);

    impl<'call, A> FromExcel<'call> for MainThreadContext<'call, A> {
        fn from_excel(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
            Ok(Self(PhantomData))
        }
    }
    impl<'call, A> FromExcel<'call> for ThreadSafeContext<'call, A> {
        fn from_excel(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
            Ok(Self(PhantomData))
        }
    }
    impl<'call, A> FromExcel<'call> for MacroSheetContext<'call, A> {
        fn from_excel(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
            Ok(Self(PhantomData))
        }
    }
}

#[excel_function]
fn main_context(context: custom::MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function]
fn thread_context(context: custom::ThreadSafeContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function]
fn macro_context(context: custom::MacroSheetContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
