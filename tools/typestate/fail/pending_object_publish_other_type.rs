// module: handle
use super::object::PendingObjectBinding;
use super::registry::HandleRegistry;

fn invalid<'registry>(
    registry: &'registry HandleRegistry,
    pending: PendingObjectBinding<'registry, u32>,
) {
    let _ = pending.publish::<u64>(registry); // error: E0107
}
