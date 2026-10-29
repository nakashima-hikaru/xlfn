// module: runtime
use crate::lifecycle::{LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(lifecycle: &LifecycleControl<'_, A>, control: &mut LifecycleAccess<'_, A>) {
    let _ = lifecycle.clear_certified_retirement(control); // error: E0061
}
