// module: excel_rtd/windows
use super::server_gate::{DeferredTerminationReservation, ServerOperationBarrier};

fn spawn(reservation: DeferredTerminationReservation<'_>, source: triomphe::Arc<ServerOperationBarrier>) {
    let Ok((prepared, ticket)) = reservation.prepare(source) else { return };
    let worker = std::thread::spawn(move || ticket.with_completion(|owner| drop(owner)));
    prepared.commit();
    let _ = worker.join();
}
