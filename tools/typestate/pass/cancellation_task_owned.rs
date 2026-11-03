// module: cancellation
use super::{TaskCancellationOwner, CancellationGuarantee};
fn valid() {
    let (owner, token) = TaskCancellationOwner::new(CancellationGuarantee::BestEffort);
    let (source, registration) = owner.into_parts();
    let _ = token.is_cancelled();
    drop(registration);
    drop(source);
}
