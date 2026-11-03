// module: runtime
use crate::generation::{OpenAttemptId, RemovalEpoch};
use crate::lifecycle::LifecycleControl;

fn observe_and_abandon<A: crate::Addin>(lifecycle: &LifecycleControl<'_, A>, epoch: RemovalEpoch) -> crate::XllResult<OpenAttemptId> {
    let start = lifecycle.begin_open(epoch)?;
    let attempt = start.attempt();
    let _ = start.access().phase();
    drop(start);
    Ok(attempt)
}
