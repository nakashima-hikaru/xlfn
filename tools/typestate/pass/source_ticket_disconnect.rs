// module: subscription/source
use super::{RtdSubscription, SourceSubscription};

fn disconnect<E: RtdSubscription>(mut ticket: SourceSubscription<E>) -> crate::XllResult<()> {
    ticket.request_cancel();
    ticket.disconnect_and_wait()
}
