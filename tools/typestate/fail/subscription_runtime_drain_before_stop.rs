// module: subscription/runtime
use super::termination::{Claimed, RuntimeTermination};
fn invalid(owner: RuntimeTermination<'_, crate::excel_rtd::RtdSubscriptionHost, (), crate::DynamicRtdSources<()>, Claimed>) {
    let _ = owner.drain_sink_users(); // error: E0599
}
