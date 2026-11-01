// module: async_udf/boundary
use super::launch::LaunchOwner;
fn invalid<A: crate::Addin>(
    guard: &crate::runtime::AsyncCallGuard<'_, A>,
    foreign: crate::async_udf::registry::CalculationSnapshot<'static>,
) {
    let _ = LaunchOwner::new(guard, foreign); // error: E0599
}
