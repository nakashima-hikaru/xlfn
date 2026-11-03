// module: excel_rtd/windows
use super::server::{PublishedServerStart, ServerStartReservation};

fn invalid(mut reserved: ServerStartReservation<'_>, mut published: PublishedServerStart<'_>) {
    reserved.server = None; // error: E0616
    published.server = None; // error: E0616
}
