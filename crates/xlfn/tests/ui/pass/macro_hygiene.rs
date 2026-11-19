use xlfn::prelude::*;

#[allow(
    unused_macros,
    reason = "reject generated code that resolves the caller's env! macro"
)]
macro_rules! env {
    ($($arguments:tt)*) => {
        compile_error!("generated add-in code must not invoke the caller's env! macro")
    };
}

#[allow(
    unused_macros,
    reason = "detect accidental resolution at the call site"
)]
macro_rules! stringify {
    ($($arguments:tt)*) => {
        compile_error!("generated registration must use core::stringify!")
    };
}

#[allow(non_camel_case_types, dead_code)]
type u8 = ();
#[allow(non_camel_case_types, dead_code)]
type i32 = ();
#[allow(non_camel_case_types, dead_code)]
type u32 = ();
#[allow(non_camel_case_types, dead_code)]
type str = ();

#[derive(ExcelEnum)]
enum Choice {
    First,
}

#[excel_function]
fn accepts_a_choice(choice: Choice) -> f64 {
    match choice {
        Choice::First => 1.0,
    }
}

struct HygieneAddin;

#[excel_addin(name = "Macro Hygiene", id = "macro-hygiene")]
impl Addin for HygieneAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext<Self>,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(()))
    }
}

// These names match generated locals. The default must still resolve the
// caller's functions, including while an injected context is in scope.
fn __frame() -> f64 {
    1.0
}
fn __state() -> f64 {
    2.0
}
fn __context() -> f64 {
    3.0
}
fn __generated_context() -> f64 {
    4.0
}
fn __execute() -> f64 {
    5.0
}

#[excel_function]
fn defaults_resolve_at_call_site(
    _context: xlfn::addin::MainThreadContext<'_, HygieneAddin>,
    #[excel_arg(default = __frame() + __state() + __context() + __generated_context() + __execute())]
    value: f64,
) -> f64 {
    value
}

fn main() {}
