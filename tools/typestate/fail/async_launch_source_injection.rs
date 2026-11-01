// module: async_udf/boundary
use super::launch::{Plain, ReservedLaunch};
use crate::async_udf::transport::ExcelAsyncResponder;
use crate::async_udf::instrumentation::NoObservation;
use crate::cancellation::CancellationSource;
fn invalid<A: crate::Addin>(launch: ReservedLaunch<'_, '_, '_, A, Plain<std::future::Ready<crate::XllResult<u32>>>>, responder: ExcelAsyncResponder, source: CancellationSource) {
    launch.commit(responder, NoObservation, source); // error: E0061
}
