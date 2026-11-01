// module: runtime
use crate::lifecycle::{LifecyclePhase, OpenStart};

fn invalid<A: crate::Addin>(phase: LifecyclePhase) {
    let _: OpenStart<'_, A> = phase; // error: E0308
}
