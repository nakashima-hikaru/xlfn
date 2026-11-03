// module: subscription/data_plane/refresh/windows
use super::publication::RefreshHandoff;
fn invalid(mut receipt: RefreshHandoff) {
    receipt.completion = Ok(()); // error: E0616
}
