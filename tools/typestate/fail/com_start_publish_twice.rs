// module: excel_rtd/windows
use super::server::ServerStartReservation;
use super::update_event::{RetainedUpdateCallback, ServerCallbacks};
use crate::sync::Mutex;

fn invalid(
    reserved: ServerStartReservation<'_>,
    callbacks: &Mutex<ServerCallbacks>,
    first: Box<RetainedUpdateCallback>,
    second: Box<RetainedUpdateCallback>,
) {
    let _ = reserved.publish_callback(callbacks, first);
    let _ = reserved.publish_callback(callbacks, second); // error: E0382
}
