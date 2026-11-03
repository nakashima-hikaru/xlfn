// module: excel_rtd/windows
use super::server::{RtdServer, SERVER_STARTED};
use std::sync::atomic::Ordering;

fn invalid(server: &RtdServer) {
    server.start_state.store(SERVER_STARTED, Ordering::Release); // error: E0616
}
