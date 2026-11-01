// module: async_udf
use super::registry::control::RegistryControl;

fn invalid(control: &mut RegistryControl) {
    control.begin_advance(); // error: E0599
}
