// module: value
fn two_witnesses(domain: &crate::handle::HandleReadDomain) {
    crate::call::with_branded_call_scope_and_state(domain, |domain, storage, mut token| {
        let first = storage.enter_handle_domain(&mut token, domain).unwrap();
        let second = storage.enter_handle_domain(&mut token, domain).unwrap();
        let _ = (first.domain(), second.domain());
    });
}
