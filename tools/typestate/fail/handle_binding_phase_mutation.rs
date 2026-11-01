// module: handle
fn invalid(record: &super::binding::BindingRecord) {
    record.state.store(1, std::sync::atomic::Ordering::Release); // error: E0616
}
