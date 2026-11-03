// module: excel_rtd/windows
use super::server::ServerStartReservation;
use super::update_event::{RetainedUpdateCallback, ServerCallbacks};
use crate::sync::Mutex;

fn invalid(reserved: ServerStartReservation<'_>, other: &Mutex<ServerCallbacks>, callback: Box<RetainedUpdateCallback>) {
    let _ = reserved.publish_callback(callback, other); // error: E0061
}
