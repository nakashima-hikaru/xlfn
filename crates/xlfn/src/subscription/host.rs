//! Host port used by the subscription engine.

use super::data_plane::PublishCore;
use super::delivery::PublishSink;
use super::topic::TopicId;
use crate::XllResult;
use crate::generation::ConnectionGeneration;

pub(crate) trait SubscriptionHost: Clone + Send + Sync + 'static {
    // Owned publish operations transfer this guard to their completing thread.
    type AdmissionGuard: Send + 'static;
    type Notifier: Clone + Send + Sync + 'static;
    // Kept inline in deferred cleanup until its sink-user barrier completes.
    type DisconnectCompletion: Send + 'static;

    fn enter_with<F>(&self, operation: F) -> XllResult<Self::AdmissionGuard>
    where
        F: FnOnce() -> XllResult<()>;

    fn notify(&self, notifier: &Self::Notifier) -> XllResult<()>;

    /// Builds the concrete source-facing adapter without erasing its host.
    fn publish_sink(
        core: &PublishCore<Self>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
    ) -> PublishSink;
}
