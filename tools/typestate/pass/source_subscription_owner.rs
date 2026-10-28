// module: subscription
use super::source::SourceSubscription;

fn require_send<T: Send>() {}

fn valid<E: crate::RtdSubscription>(mut ticket: SourceSubscription<E>) {
    require_send::<SourceSubscription<E>>();
    ticket.request_cancel();
    let _ = ticket.disconnect_and_wait();
}
