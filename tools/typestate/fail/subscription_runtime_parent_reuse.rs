// module: subscription/runtime
use super::termination::{Claimed, RuntimeTermination};
fn invalid(owner: RuntimeTermination<'_, crate::excel_rtd::RtdSubscriptionHost, (), crate::DynamicRtdSources<()>, Claimed>) {
    drop(owner.request_stop());
    let _ = owner.request_stop(); // error: E0382
}
