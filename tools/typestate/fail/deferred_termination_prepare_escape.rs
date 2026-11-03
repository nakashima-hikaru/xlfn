// module: excel_rtd/windows
use super::server_gate::{DeferredTerminationReservation, ServerOperationBarrier};

fn invalid(reservation: DeferredTerminationReservation<'_>, source: triomphe::Arc<ServerOperationBarrier>) {
    let _ = reservation.prepare(source); // error: E0624
}
