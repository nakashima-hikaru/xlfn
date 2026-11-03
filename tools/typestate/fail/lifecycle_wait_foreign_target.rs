// module: runtime
use crate::lifecycle::{LifecycleAccess, LifecycleCoordinator};

fn invalid<A: crate::Addin>(access: &mut LifecycleAccess<'_, A>, other: &LifecycleCoordinator<A>) {
    access.wait_for_change(other); // error: E0061
}
