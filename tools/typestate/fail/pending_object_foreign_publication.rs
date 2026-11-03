// module: handle
use super::object::PendingObjectBinding;
use super::registry::HandleRegistry;

fn invalid<'registry>(
    pending: PendingObjectBinding<'registry, u32>,
    other: &'registry HandleRegistry,
) {
    let _ = pending.publish(other); // error: E0061
}
