// module: async_udf/instrumentation
use super::{AsyncCompletion, AsyncObservation, CompletionObserver, UdfLayerGuard};

fn finish<G: UdfLayerGuard>(observation: AsyncObservation<G>, completion: &AsyncCompletion) {
    observation.finish(completion);
}
