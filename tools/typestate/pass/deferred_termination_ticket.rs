// module: excel_rtd/windows
use super::server::RtdServer;
use super::server_gate::DeferredTerminationReservation;

fn spawn(reservation: DeferredTerminationReservation<'_>, source: &RtdServer) {
    let Ok(start) = reservation.start(source) else { return };
    let _ = start.spawn();
}
