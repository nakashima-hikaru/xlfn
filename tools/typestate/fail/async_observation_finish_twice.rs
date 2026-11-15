// module: async_udf/instrumentation
use super::{AsyncCompletion, AsyncObservation, CompletionObserver, UdfLayerGuard};

fn finish_twice<G: UdfLayerGuard>(observation: AsyncObservation<G>, completion: &AsyncCompletion) {
    observation.finish(completion);
    observation.finish(completion); // error: E0382
}
