// module: excel_rtd/windows/server_gate
use super::DeferredTerminationReservation;

fn commit_twice(reservation: DeferredTerminationReservation<'_>) {
    reservation.commit();
    reservation.commit(); // error: E0382
}
