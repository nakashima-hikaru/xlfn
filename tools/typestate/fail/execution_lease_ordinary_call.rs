// module: runtime
use super::{CallGuard};
fn invalid<A: crate::Addin>(call: &CallGuard<'_, A>) {
    let _ = call.try_execution_lease(); // error: E0599
}
