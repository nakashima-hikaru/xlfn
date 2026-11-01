// module: excel_rtd/windows/server
fn invalid(server: &super::RtdServer<(), crate::DynamicRtdSources<()>>, ticket: super::DeferredTerminationTicket) {
    let _work = server.deferred_termination_work(ticket); // error: E0599
}
