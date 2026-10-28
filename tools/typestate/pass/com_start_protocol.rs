// module: excel_rtd/windows
use super::server::ServerStartReservation;
use super::update_event::{RetainedUpdateCallback, ServerCallbacks};
use crate::sync::Mutex;

fn start(
    reserved: ServerStartReservation<'_>,
    callbacks: &Mutex<ServerCallbacks>,
    callback: Box<RetainedUpdateCallback>,
) {
    let (published, _) = reserved.publish_callback(callbacks, callback);
    published.commit();
}
