// module: async_udf/boundary
use super::launch::CapturedLaunch;
fn valid<A: crate::Addin>(guard: &crate::runtime::AsyncCallGuard<'_, A>) {
    let captured = CapturedLaunch::capture(guard);
    let _ = captured.calculation_id();
    let _ = captured.into_launch();
}
