// module: subscription/runtime
use crate::subscription::{server::SubscriptionServerHandle, topic::{TopicId, SubscriptionId}};
use crate::subscription::host::SubscriptionHost;
#[cfg(not(windows))]
fn bound<H: SubscriptionHost>(handle: &SubscriptionServerHandle<H>, topic: TopicId, id: SubscriptionId) -> crate::XllResult<()> {
    handle.connect_transaction(topic, id)?.rollback();
    handle.disconnect(topic)
}
#[cfg(windows)]
fn bound<H: SubscriptionHost>(
    handle: &SubscriptionServerHandle<H>,
    topic: TopicId,
    id: SubscriptionId,
    completion: H::DisconnectCompletion,
) -> crate::XllResult<()> {
    handle.connect_transaction(topic, id)?.rollback();
    handle.disconnect_deferred(topic, completion)
}
