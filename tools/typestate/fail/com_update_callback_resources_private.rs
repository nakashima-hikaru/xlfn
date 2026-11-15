// module: excel_rtd/windows
use super::update_event::RetainedUpdateCallback;

fn invalid(callback: RetainedUpdateCallback) {
    let _ = &callback.resources; // error: E0616
}
