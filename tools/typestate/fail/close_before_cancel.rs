// module: async_udf
use super::registry::{CancellationPending, RegistryClose};

fn invalid(close: RegistryClose<'_, CancellationPending>) {
    close.wait(); // error: E0599
}
