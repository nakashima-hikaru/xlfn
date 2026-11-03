// module: excel_rtd/windows
use super::server::ServerStartReservation;
use super::update_event::RetainedUpdateCallback;

fn invalid(
    reserved: ServerStartReservation<'_>,
    first: Box<RetainedUpdateCallback>,
    second: Box<RetainedUpdateCallback>,
) {
    let _ = reserved.publish_callback(first);
    let _ = reserved.publish_callback(second); // error: E0382
}
