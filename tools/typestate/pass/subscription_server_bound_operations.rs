// module: subscription/runtime
use crate::subscription::{server::SubscriptionServerHandle, topic::{TopicId, SubscriptionId}};
use crate::subscription::host::SubscriptionHost;
fn bound<H: SubscriptionHost>(handle: &SubscriptionServerHandle<H>, topic: TopicId, id: SubscriptionId) -> crate::XllResult<()> {
    handle.connect_transaction(topic, id)?.rollback();
    handle.disconnect(topic)
}
