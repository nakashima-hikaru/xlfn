// module: async_udf/boundary
use super::launch::CapturedLaunch;
fn invalid<A: crate::Addin>(captured: CapturedLaunch<'_, '_, A>) {
    let _ = captured.into_launch();
    let _ = captured.into_launch(); // error: E0382
}
