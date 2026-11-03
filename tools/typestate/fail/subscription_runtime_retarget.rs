// module: subscription/runtime
use super::SubscriptionRuntime;
use super::termination::{Claimed, RuntimeTermination};
fn invalid<'a>(mut owner: RuntimeTermination<'a, crate::excel_rtd::RtdSubscriptionHost, (), crate::DynamicRtdSources<()>, Claimed>, other: &'a SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost>) {
    owner.runtime = other; // error: E0616
}
