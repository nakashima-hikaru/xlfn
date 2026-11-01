// module: async_udf/boundary
use super::launch::{Scoped, ReservedLaunch};
use crate::async_udf::task_scope::HandleScopedBuilder;
use crate::async_udf::transport::ExcelAsyncResponder;
use crate::async_udf::instrumentation::NoObservation;
fn invalid<A: crate::Addin, B: HandleScopedBuilder<u32> + Send + 'static>(launch: ReservedLaunch<'_, '_, '_, A, Scoped<B>>, responder: ExcelAsyncResponder, generation: crate::generation::RuntimeGeneration) {
    launch.commit::<u32, _>(responder, NoObservation, generation); // error: E0061
}
