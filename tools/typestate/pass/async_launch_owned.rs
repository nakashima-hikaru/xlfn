// module: async_udf/boundary
use super::launch::{Plain, ReservedLaunch, PreparedLaunch};
use crate::async_udf::transport::ExcelAsyncResponder;
use crate::async_udf::instrumentation::NoObservation;
fn prepare<A: crate::Addin>(launch: PreparedLaunch<'_, '_, '_, A, ()>) { let _ = launch.reserve(); }
fn commit<A: crate::Addin>(launch: ReservedLaunch<'_, '_, '_, A, Plain<std::future::Ready<crate::XllResult<u32>>>>, responder: ExcelAsyncResponder) {
    launch.commit(responder, NoObservation);
}
