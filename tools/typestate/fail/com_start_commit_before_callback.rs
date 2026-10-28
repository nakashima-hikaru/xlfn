// module: excel_rtd/windows
use super::server::ServerStartReservation;

fn invalid(reserved: ServerStartReservation<'_>) {
    reserved.commit(); // error: E0599
}
