// module: runtime
use crate::lifecycle::{LifecycleAccess, LifecycleCoordinator};

fn invalid<A: crate::Addin>(coordinator: &LifecycleCoordinator<A>, access: &mut LifecycleAccess<'_, A>) {
    let _ = coordinator.claim_removal(access); // error: E0624
}
