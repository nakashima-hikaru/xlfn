// module: subscription/runtime
use super::termination::{RuntimeTermination, SinkUsersDrained};
fn invalid(owner: RuntimeTermination<'_, crate::excel_rtd::RtdSubscriptionHost, (), crate::DynamicRtdSources<()>, SinkUsersDrained>) {
    let _ = owner.finish(); // error: E0599
}
