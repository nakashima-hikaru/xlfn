// module: registration
fn raw_status() -> crate::host_api::HostInvocation<()> {
    crate::host_api::HostInvocation::Failed {
        status: crate::return_abi::ExcelCallbackStatus::Success, // error: E0308
        cleanup: Ok(()),
    }
}
