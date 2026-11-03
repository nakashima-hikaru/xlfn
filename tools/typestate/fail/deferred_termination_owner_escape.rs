// module: excel_rtd/windows
use super::server_gate::{DeferredTerminationTicket, ServerTermination};

fn invalid(ticket: DeferredTerminationTicket, output: &mut Option<ServerTermination<'static>>) {
    let _ = ticket.with_completion(|termination| {
        *output = Some(termination); // error: E0521
    });
}
