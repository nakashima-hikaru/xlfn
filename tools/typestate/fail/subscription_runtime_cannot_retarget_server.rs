// module: subscription/runtime
use super::SubscriptionRuntime;
use crate::subscription::{server::SubscriptionServerHandle, topic::{TopicId, SubscriptionId}};
use crate::subscription::host::SubscriptionHost;
fn cannot_mix<H: SubscriptionHost>(
    runtime: &SubscriptionRuntime<H>, handle: &SubscriptionServerHandle<H>,
    topic: TopicId, id: SubscriptionId,
) {
    let _ = runtime.connect_transaction(handle, topic, id); // error: E0599
    let _ = runtime.disconnect(handle, topic); // error: E0599
}
