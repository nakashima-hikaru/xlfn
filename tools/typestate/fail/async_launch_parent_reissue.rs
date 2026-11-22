// module: async_udf/boundary
use super::launch::{LaunchOwner, PlainStart};
use crate::async_udf::instrumentation::NoObservation;
use crate::async_udf::transport::ExcelAsyncResponder;
fn prepare<A: crate::Addin>(
    _: &crate::runtime::CallGuard<'_, A>,
    _: crate::generation::ExecutionLease<A>,
    _: crate::cancellation::CancellationToken,
) -> crate::XllResult<std::future::Ready<crate::XllResult<u32>>> {
    Ok(std::future::ready(Ok(1)))
}
fn invalid<A: crate::Addin>(launch: LaunchOwner<'_, '_, A>, first: ExcelAsyncResponder, second: ExcelAsyncResponder) {
    let child = launch.prepare(PlainStart(prepare::<A>), first, NoObservation);
    drop(child.and_then(|prepared| prepared.reserve()));
    let _ = launch.prepare(PlainStart(prepare::<A>), second, NoObservation); // error: E0382
}
