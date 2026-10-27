// module: async_udf
use super::registry::{RegistryClose, TasksCancelled};

fn invalid(close: RegistryClose<'_, TasksCancelled>) {
    close.finish(); // error: E0599
}
