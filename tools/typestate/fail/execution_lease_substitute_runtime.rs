// module: runtime
use super::{AsyncCallGuard, Runtime};

fn invalid<A: crate::Addin>(call: &AsyncCallGuard<'_, A>, other: &'static Runtime<A>) {
    let _ = call.try_execution_lease(other); // error: E0061
}
