// module: async_udf
use super::registry::control::RegistryControl;

fn invalid(control: &RegistryControl) {
    let _ = &control.state; // error: E0616
}
