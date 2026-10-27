// module: subscription
use super::source::SourceSubscription;

fn invalid(ticket: SourceSubscription) {
    let _ = ticket.disconnect_and_wait();
    let _ = ticket.disconnect_and_wait(); // error: E0382
}
