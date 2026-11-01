// module: async_udf/boundary
use super::launch::LaunchOwner;
use crate::async_udf::instrumentation::NoObservation;
use crate::async_udf::transport::ExcelAsyncResponder;
fn invalid<A: crate::Addin>(launch: LaunchOwner<'_, '_, A>, first: ExcelAsyncResponder, second: ExcelAsyncResponder) {
    let child = launch.prepare(|_, _, _| Ok(std::future::ready(Ok::<u32, crate::XllError>(1_u32))), first, NoObservation);
    drop(child.and_then(|prepared| prepared.reserve()));
    let _ = launch.prepare(|_, _, _| Ok(std::future::ready(Ok::<u32, crate::XllError>(2_u32))), second, NoObservation); // error: E0382
}
