// module: registration
fn forge() {
    let _ = crate::host_api::HostFailureStatus(crate::return_abi::ExcelCallbackStatus::Success); // error: E0603
}
