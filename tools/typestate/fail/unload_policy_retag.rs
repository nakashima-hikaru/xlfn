// module: runtime
use super::{LogicalUnload, PhysicalUnload, Runtime};

fn retag<A: crate::addin_impl::PhysicallyUnloadableAddin>(
    runtime: Runtime<A, LogicalUnload>,
) -> Runtime<A, PhysicalUnload> {
    runtime // error: E0308
}
