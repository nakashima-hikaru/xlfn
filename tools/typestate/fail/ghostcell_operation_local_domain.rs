// module: value
fn cannot_register_local_domain() {
    crate::call::with_branded_call_scope(|storage, mut token| {
        let domain = crate::handle::HandleReadDomain::new();
        let _ = storage.enter_handle_domain(&mut token, &domain); // error: E0597
    });
}
