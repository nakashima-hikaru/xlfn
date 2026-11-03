// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, SubscriptionHost};
use super::publication::RefreshPublication;
use crate::excel_rtd::WindowsRefreshArrayApi;
fn invalid<'a, H: SubscriptionHost>(mut ready: RefreshPublication<'a, H, WindowsRefreshArrayApi>, other: RefreshPublication<'a, H, WindowsRefreshArrayApi>, batch: RtdRefreshBatch<'a, H>) {
    ready.batch = batch; // error: E0616
    let other_array = other.array; // error: E0616
    ready.array = other_array; // error: E0616
}
