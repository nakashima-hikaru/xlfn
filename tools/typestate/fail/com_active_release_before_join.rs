// module: excel_rtd/windows
use super::server::QuiescentServerAccess;

fn invalid(access: QuiescentServerAccess<'_>) {
    let _ = access.teardown_and_remove_active(false); // error: E0599
}
