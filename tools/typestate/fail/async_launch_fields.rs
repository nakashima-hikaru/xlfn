// module: async_udf/boundary
use super::launch::LaunchOwner;
fn invalid<A: crate::Addin>(mut launch: LaunchOwner<'_, '_, A>) {
    let _ = launch.guard; // error: E0616
    let _ = launch.calculation.take(); // error: E0616
    let _ = launch.cancellation.take(); // error: E0616
}
