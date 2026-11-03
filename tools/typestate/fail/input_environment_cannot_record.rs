// module: macro_abi
use crate::__private::v1::*;
fn environment_has_no_recorder(mut environment: CallEnvironment<'_>) {
    let _ = environment.native_f64(0, "number", 1.0); // error: E0599
}
