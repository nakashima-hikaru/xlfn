#![cfg_attr(
    all(not(target_os = "windows"), feature = "rtd"),
    allow(dead_code, reason = "Internal helpers for Windows COM integration")
)]
#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The private subscription backend shares the formula-handle COM transport"
    )
)]

#[cfg(feature = "rtd")]
mod budget;
mod catalog;
#[cfg(feature = "rtd")]
mod channel;
mod data_plane;
mod delivery;
mod disconnect;
mod host;
mod identity;
#[cfg(feature = "rtd")]
mod multiplex;
mod runtime;
mod runtime_services;
mod server;
pub(crate) mod source;
mod termination;
mod topic;
mod value;

pub(crate) type PublishSink = delivery::PublishSink;
pub(crate) use data_plane::PublishCore;
pub(crate) use topic::TopicId as SinkTopicId;
#[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
pub(crate) type RtdRefreshBatch<'a> =
    data_plane::RtdRefreshBatch<'a, crate::excel_rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionRuntime<E = (), C = crate::DynamicRtdSources<E>> =
    runtime::SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost, E, C>;
pub(crate) type SubscriptionServerHandle<E = (), C = crate::DynamicRtdSources<E>> =
    server::SubscriptionServerHandle<crate::excel_rtd::RtdSubscriptionHost, E, C>;

#[cfg(feature = "rtd")]
pub use budget::{RtdDelivery, RtdDeliveryPolicy, RtdFifo, RtdLatest, RtdSourceLimits};
#[cfg(feature = "rtd")]
pub use channel::{
    RtdBatchSendError, RtdChannelSource, RtdChannelSubscription, RtdPendingBatch, RtdPendingValue,
    RtdProducerErrorPolicy, RtdSendError, RtdSender,
};
#[cfg(feature = "rtd")]
pub use multiplex::{
    NoTopicValidator, RtdMultiplexedSource, RtdMultiplexedSubscription, RtdSourceContext,
    RtdSourceEvent, RtdSubscriptionId, RtdTopicValidator,
};
#[cfg(feature = "rtd")]
pub use source::{IntoRtdSubscription, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription};
#[cfg(not(feature = "rtd"))]
pub(crate) use topic::RtdTopic;
#[cfg(feature = "rtd")]
pub use topic::{RtdCapacity, RtdLimits};
#[cfg(feature = "rtd")]
pub use topic::{RtdTopic, RtdTopicInput, RtdTopicParts};
#[cfg(feature = "rtd")]
pub use value::IntoRtdValue;
#[cfg(feature = "rtd")]
pub use value::RtdValue;
#[cfg(not(feature = "rtd"))]
pub(crate) use value::RtdValue;

#[cfg(any(
    all(test, feature = "rtd"),
    all(feature = "bench-internals", feature = "rtd"),
    all(target_os = "windows", any(feature = "rtd", feature = "handles")),
))]
pub(crate) use crate::generation::ServerGeneration;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use crate::generation::{ConnectionGeneration, RuntimeGeneration};
#[cfg(any(
    all(test, feature = "rtd"),
    all(feature = "bench-internals", feature = "rtd"),
    all(target_os = "windows", any(feature = "rtd", feature = "handles")),
))]
pub(crate) use delivery::RefreshOutcome;
#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles"),))]
pub(crate) use delivery::RtdUpdate;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use delivery::{RefreshState, ValueSlot, shard_index};

#[cfg(any(
    all(test, feature = "rtd"),
    all(feature = "bench-internals", feature = "rtd")
))]
pub(crate) use delivery::TOPIC_SHARDS;
pub(crate) use host::SubscriptionHost;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use identity::SubscriptionIdentityIndex;
pub(crate) use runtime_services::RuntimeServices;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use server::PANIC_AFTER_TERMINATION_GUARD;
#[cfg(all(test, feature = "rtd"))]
use server::TerminationAdmission;
#[cfg(all(feature = "rtd", feature = "bench-internals"))]
pub(crate) use source::SourceSubscription;
#[cfg(feature = "rtd")]
pub(crate) use source::{SourceArena, SourceRegistration};
#[cfg(all(feature = "rtd", any(test, feature = "bench-internals")))]
pub(crate) use topic::BorrowedTopicParts;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use topic::SubscriptionId;
pub(crate) use topic::SubscriptionKey;
#[cfg(any(
    all(test, feature = "rtd"),
    all(feature = "bench-internals", feature = "rtd"),
    all(target_os = "windows", any(feature = "rtd", feature = "handles")),
))]
pub(crate) use topic::TopicId;
#[cfg(all(test, feature = "rtd"))]
pub(crate) use topic::{SourceId, SubscriptionIdentityKey};
#[cfg(any(
    all(test, feature = "rtd"),
    all(target_os = "windows", any(feature = "rtd", feature = "handles")),
    all(feature = "bench-internals", feature = "rtd"),
))]
pub(crate) use value::StoredRtdValue;
#[cfg(all(test, feature = "rtd"))]
pub(crate) mod tests;
