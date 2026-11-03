// module: runtime
use crate::lifecycle::OpenStart;

fn invalid<A: crate::Addin>(start: OpenStart<'_, A>) {
    let _guard = *start.access(); // error: E0507
}
