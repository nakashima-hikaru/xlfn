// module: async_udf/boundary
use super::launch::PreparedLaunch;
fn invalid<A: crate::Addin, B>(launch: PreparedLaunch<'_, '_, '_, A, B>) {
    let _ = launch.reserve();
    let _ = launch.reserve(); // error: E0382
}
