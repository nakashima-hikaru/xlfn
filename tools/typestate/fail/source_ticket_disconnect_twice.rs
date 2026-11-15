// module: subscription/source
use super::{RtdSubscription, SourceSubscription};

fn disconnect_twice<E: RtdSubscription>(ticket: SourceSubscription<E>) {
    let _ = ticket.disconnect_and_wait();
    let _ = ticket.disconnect_and_wait(); // error: E0382
}
