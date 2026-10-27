// module: subscription
use super::source::SourceSubscription;

fn require_send<T: Send>() {}

fn valid(mut ticket: SourceSubscription) {
    require_send::<SourceSubscription>();
    ticket.request_cancel();
    let _ = ticket.disconnect_and_wait();
}
