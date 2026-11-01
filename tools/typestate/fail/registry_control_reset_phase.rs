// module: async_udf
use super::registry::control::RegistryControl;

fn invalid(control: &mut RegistryControl) {
    let _ = &mut control.state; // error: E0616
}
