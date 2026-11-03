// module: excel_rtd/windows/server
fn invalid<'a>(server: &'a super::RtdServer<(), crate::DynamicRtdSources<()>>, termination: super::ServerTermination<'a>) {
    let _access = super::QuiescentServerAccess::from_termination(server, termination); // error: E0599
}
