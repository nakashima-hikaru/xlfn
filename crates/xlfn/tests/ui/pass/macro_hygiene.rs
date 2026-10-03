use xlfn::prelude::*;

#[allow(unused_macros, reason = "reject generated code that resolves the caller's env! macro")]
macro_rules! env {
    ($($arguments:tt)*) => {
        compile_error!("generated add-in code must not invoke the caller's env! macro")
    };
}

#[excel_addin(name = "Macro Hygiene", id = "macro-hygiene")]
struct HygieneAddin;

impl Addin for HygieneAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(()))
    }
}

fn main() {}
