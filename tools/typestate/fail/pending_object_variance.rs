// module: handle
use super::object::PendingObjectBinding;

fn invalid<'registry>(
    pending: PendingObjectBinding<'registry, for<'a> fn(&'a str)>,
) -> PendingObjectBinding<'registry, fn(&'static str)> {
    pending // error: E0308
}
