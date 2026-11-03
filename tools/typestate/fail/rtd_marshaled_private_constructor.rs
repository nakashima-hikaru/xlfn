// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, SubscriptionHost};
use super::publication::RefreshPublication;
use crate::excel_rtd::WindowsRefreshArrayApi;
fn invalid<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>) {
    let _ = RefreshPublication::<H, WindowsRefreshArrayApi>::prepare(batch); // error: E0624
}
