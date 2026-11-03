// module: boundary/host
fn cannot_escape() {
    let mut escaped: Option<super::HostCloseBoundary<'static>> = None;
    super::with_host_lifecycle(std::ptr::null(), |frame| {
        escaped = Some(super::HostCloseBoundary { frame }); // error: E0521
    });
    drop(escaped);
}
