#![deny(unsafe_code)]

use xlfn::prelude::*;

#[excel_addin(name = "Opaque body", id = "opaque-body")]
struct App;

impl Addin for App {
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

#[excel_function(thread_safe)]
fn nested_syntax(value: f64) -> f64 {
    #![allow(unused_braces)]
    macro_rules! capture {
        () => { value };
    }
    let evaluate = || {
        'result: {
            let [head, ..] = [capture!(), 2.0];
            break 'result head;
        }
    };
    evaluate()
}

fn main() {
    assert_eq!(nested_syntax(3.0), 3.0);
}
