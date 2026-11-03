// module: cancellation
use super::TaskCancellationOwner;
fn invalid(owner: TaskCancellationOwner) {
    let _ = owner.into_parts();
    let _ = owner.into_parts(); // error: E0382
}
