// module: subscription
use super::source::SourceSubscription;

fn invalid<E: crate::RtdSubscription>(ticket: SourceSubscription<E>) {
    let _ = ticket.disconnect_and_wait();
    let _ = ticket.disconnect_and_wait(); // error: E0382
}
