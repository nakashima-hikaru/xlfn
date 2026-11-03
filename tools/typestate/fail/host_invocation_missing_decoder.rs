// module: registration
fn missing_decoder() -> crate::host_api::HostInvocation<()> {
    crate::host_api::HostInvocation::Decoded { cleanup: Ok(()) } // error: E0063
}
