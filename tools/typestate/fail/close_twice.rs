// module: async_udf
use super::registry::{RegistryClose, TasksDrained};

fn invalid(close: RegistryClose<'_, TasksDrained>) {
    close.finish();
    close.finish(); // error: E0382
}
