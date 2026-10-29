// module: runtime
use super::{Runtime};
fn invalid<'call, A: crate::Addin>(
    runtime: &'call Runtime<A>,
    ingress: &'call crate::ingress::AdmittedExport<'call>,
) {
    let _ = runtime.enter_async(ingress); // error: E0521
}
