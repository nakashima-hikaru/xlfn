// module: runtime
use crate::lifecycle::{LifecycleAccess, LifecycleControl};

fn invalid<A: crate::Addin>(control: &LifecycleControl<'_, A>, other_access: &mut LifecycleAccess<'_, A>) {
    control.wait(other_access); // error: E0599
}
