use xlfn::prelude::*;

#[excel_addin(name = "Callback Authority", id = "callback-authority")]
struct CallbackAddin;

impl Addin for CallbackAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
        Ok(Opened::new(()))
    }
}

static OTHER_RUNTIME: xlfn::__private::v1::MacroRuntime<CallbackAddin> =
    xlfn::__private::v1::MacroRuntime::new();

fn main() {
    xlAutoOpen();
    xlAutoClose();
    xlAutoRemove();

    xlfn::__private::v1::open_generated_addin(
        &OTHER_RUNTIME,
        "callback-authority",
        "Callback Authority",
        "Test",
        "0.0.0",
        "test",
        std::ptr::null(),
    );
    xlfn::__private::v1::auto_close_generated_addin(&OTHER_RUNTIME);
    xlfn::__private::v1::auto_remove_generated_addin(&OTHER_RUNTIME);
}
