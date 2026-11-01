// module: async_udf/boundary
use super::launch::{Plain, ReservedLaunch};
use crate::async_udf::instrumentation::NoObservation;
use crate::async_udf::transport::ExcelAsyncResponder;
fn invalid<A: crate::Addin>(launch: ReservedLaunch<'_, '_, A, Plain<std::future::Ready<crate::XllResult<u32>>>, NoObservation>, other: ExcelAsyncResponder) {
    launch.commit(other); // error: E0061
}
