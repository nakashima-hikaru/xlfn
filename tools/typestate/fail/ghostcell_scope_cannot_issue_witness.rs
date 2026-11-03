// module: value
fn cannot_issue<'call>(scope: &'call crate::call::CallScope<'call>, domain: &'call crate::handle::HandleReadDomain) {
    let _ = scope.enter_handle_domain(domain); // error: E0599
}
