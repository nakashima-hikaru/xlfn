// module: async_udf
use super::control::RegistryControl;

fn invalid(control: &RegistryControl) {
    let _ = &control.phase; // error: E0616
}
