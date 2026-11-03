// module: excel_rtd/windows
use super::server::{QuiescentServerAccess, RtdServer};

fn invalid(access: QuiescentServerAccess<'_>, other: &RtdServer) {
    let _ = access.teardown_resources(other); // error: E0061
}
