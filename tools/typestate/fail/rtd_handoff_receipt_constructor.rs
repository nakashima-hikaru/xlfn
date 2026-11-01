// module: subscription/data_plane/refresh/windows
use super::publication::RefreshHandoff;
fn invalid() -> RefreshHandoff {
    RefreshHandoff { completion: Ok(()) } // error: E0451
}
