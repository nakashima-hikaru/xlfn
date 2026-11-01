// module: cancellation
use super::TaskCancellationAuthority;
fn invalid(authority: TaskCancellationAuthority) {
    let _ = authority.into_task(); // error: E0599
}
