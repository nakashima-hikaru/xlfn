// module: runtime
fn invalid(protocol: &crate::runtime_components::ReturnProtocol) {
    protocol.returns.reopen_admission().unwrap(); // error: E0616
}
