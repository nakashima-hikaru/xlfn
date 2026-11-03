// module: async_udf/boundary
use super::launch::PreparedLaunch;
use crate::async_udf::instrumentation::NoObservation;
fn invalid<A: crate::Addin, B>(launch: PreparedLaunch<'_, '_, A, B, NoObservation>) {
    let _ = launch.reserve();
    let _ = launch.reserve(); // error: E0382
}
