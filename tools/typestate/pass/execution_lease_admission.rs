// module: runtime
use super::{Runtime};
fn lease<'call, A: crate::Addin>(
    runtime: &'static Runtime<A>,
    ingress: &'call crate::ingress::AdmittedExport<'call>,
) -> crate::XllResult<crate::generation::ExecutionLease<A>> {
    let call = runtime.enter_async(ingress)?;
    call.try_execution_lease()
}
