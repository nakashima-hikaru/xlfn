// module: runtime
use crate::generation::OpenAttemptId;
use crate::lifecycle::{LifecycleAccess, LifecycleCoordinator};

fn invalid<A: crate::Addin>(coordinator: &LifecycleCoordinator<A>, access: &mut LifecycleAccess<'_, A>, attempt: OpenAttemptId) {
    coordinator.prepare_open(access); // error: E0624
    let _ = coordinator.allocate_open_attempt(access); // error: E0624
    coordinator.begin_opening(access, attempt); // error: E0624
}
