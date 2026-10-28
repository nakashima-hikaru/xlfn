// module: excel_rtd/windows
use super::server::{PublishedServerStart, ServerStartReservation};

fn invalid(mut reserved: ServerStartReservation<'_>, mut published: PublishedServerStart<'_>) {
    reserved.state = None; // error: E0616
    published.state = None; // error: E0616
}
