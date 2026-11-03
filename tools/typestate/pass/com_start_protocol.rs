// module: excel_rtd/windows
use super::server::ServerStartReservation;
use super::update_event::RetainedUpdateCallback;

fn start(
    reserved: ServerStartReservation<'_>,
    callback: Box<RetainedUpdateCallback>,
) {
    let (published, _) = reserved.publish_callback(callback);
    published.commit();
}
