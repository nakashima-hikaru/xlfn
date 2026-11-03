// module: handle
use super::object::PendingObjectBinding;

fn invalid<'registry>(
    pending: PendingObjectBinding<'registry, u32>,
) -> PendingObjectBinding<'registry, u64> {
    pending // error: E0308
}
