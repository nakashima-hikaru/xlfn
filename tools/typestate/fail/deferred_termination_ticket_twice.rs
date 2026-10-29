// module: excel_rtd/windows
use super::server_gate::DeferredTerminationTicket;

fn invalid(ticket: DeferredTerminationTicket) {
    let _ = ticket.with_completion(|owner| drop(owner));
    let _ = ticket.with_completion(|owner| drop(owner)); // error: E0382
}
