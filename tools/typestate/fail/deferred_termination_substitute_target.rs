// module: excel_rtd/windows
use super::server_gate::{DeferredTerminationTicket, ServerOperationBarrier};

fn invalid(ticket: DeferredTerminationTicket, other: &ServerOperationBarrier) {
    let _ = ticket.with_completion(|owner| drop(owner), other); // error: E0061
}
