// module: registration
fn classify<T>(invocation: crate::host_api::HostInvocation<T>) -> bool {
    use crate::host_api::HostInvocation;
    match invocation {
        HostInvocation::Suppressed { .. } => false,
        HostInvocation::Decoded { decoded, .. } => decoded.is_ok(),
        HostInvocation::Failed { status, .. } => {
            status.status() == crate::return_abi::ExcelCallbackStatus::Abort
        }
    }
}
