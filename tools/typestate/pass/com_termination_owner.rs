// module: excel_rtd/windows/server
fn ordered(server: &super::RtdServer<(), crate::DynamicRtdSources<()>>) -> i32 {
    match server.request_terminal() { Ok(owner) => owner.finish(), Err(_) => super::E_FAIL }
}
