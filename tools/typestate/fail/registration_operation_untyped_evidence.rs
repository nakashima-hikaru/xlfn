// module: registration/host
use super::{HostInvocation, Unregister, mutation_from_invocation};

fn untyped(result: HostInvocation<()>) {
    let _ = mutation_from_invocation::<Unregister>(result); // error: E0308
}
