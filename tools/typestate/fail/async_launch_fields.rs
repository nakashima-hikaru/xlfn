// module: async_udf/boundary
use super::launch::LaunchOwner;
fn invalid<A: crate::Addin>(launch: LaunchOwner<'_, '_, A>) {
    let _ = launch.guard; // error: E0616
    let _ = launch.calculation; // error: E0616
    let _ = launch.cancellation; // error: E0616
}
