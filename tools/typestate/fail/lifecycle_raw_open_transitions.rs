// module: runtime
use crate::generation::OpenAttemptId;
use crate::lifecycle::{LifecycleAccess, LifecycleCoordinator};

fn invalid<A: crate::Addin>(coordinator: &LifecycleCoordinator<A>, access: &mut LifecycleAccess<'_, A>, attempt: OpenAttemptId) {
    coordinator.prepare_open(access); // error: E0599
    let _ = coordinator.allocate_open_attempt(access); // error: E0599
    coordinator.begin_opening(access, attempt); // error: E0599
    coordinator.begin_opening_if_closed(access, crate::generation::RemovalEpoch::new(0)); // error: E0624
}
