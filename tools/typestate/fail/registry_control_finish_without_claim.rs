// module: async_udf
use super::registry::control::RegistryControl;

fn invalid(control: &mut RegistryControl) {
    control.finish_close(); // error: E0599
}
