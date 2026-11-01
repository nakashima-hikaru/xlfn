// module: cancellation
use super::CancellationSource;
fn invalid(source: CancellationSource) {
    let _ = source.task_registration(); // error: E0599
}
