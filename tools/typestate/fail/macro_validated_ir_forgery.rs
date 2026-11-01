// module: macros/lib
use super::model::{UdfSpec, UdfSpecData};
fn invalid(data: UdfSpecData) -> UdfSpec {
    UdfSpec { data } // error: E0451
}
