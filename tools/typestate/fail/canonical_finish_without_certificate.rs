// module: runtime
use crate::generation::RemovalAttemptId;
use crate::lifecycle::{LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(lifecycle: &LifecycleControl<'_, A>, control: &mut LifecycleAccess<'_, A>, attempt: RemovalAttemptId) {
    let _ = lifecycle.finish_final_removal(control, attempt); // error: E0308
    let _ = lifecycle.finish_open_rollback(control, attempt); // error: E0308
}
