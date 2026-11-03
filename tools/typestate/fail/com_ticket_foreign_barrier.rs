// module: excel_rtd/windows/server_gate
fn invalid(reservation: super::DeferredTerminationReservation<'_>, other: triomphe::Arc<super::ServerOperationBarrier>) {
    let _ = reservation.prepare(other); // error: E0061
}
