// module: runtime
use crate::lifecycle::LifecycleAccess;

fn invalid<A: crate::Addin>(mut access: LifecycleAccess<'_, A>) {
    access.clear_completed_removal_intent(); // error: E0599
}
