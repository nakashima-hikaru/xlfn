// module: runtime
use crate::lifecycle::OpenStart;

fn invalid<A: crate::Addin>(mut start: OpenStart<'_, A>) {
    let _ = start.control.take(); // error: E0616
}
