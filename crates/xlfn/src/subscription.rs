#![cfg_attr(
    all(not(target_os = "windows"), feature = "rtd"),
    allow(dead_code, reason = "Internal helpers for Windows COM integration")
)]
#![cfg_attr(
    not(feature = "rtd"),
    allow(
        dead_code,
        unreachable_pub,
        reason = "The subscription implementation is private in core-only builds"
    )
)]

mod catalog;
#[cfg(feature = "rtd")]
mod channel;
mod data_plane;
mod delivery;
mod host;
mod identity;
mod runtime;
mod runtime_services;
mod server;
mod source;
mod topic;
#[cfg(all(feature = "rtd", feature = "bench-internals"))]
mod topology_bench;
#[cfg(all(feature = "rtd", feature = "bench-internals"))]
pub use topology_bench::shared_publisher_topology_probe;
mod value;

pub(crate) type ErasedSink = delivery::ErasedSink;
pub(crate) type SubscriptionRuntime =
    runtime::SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionConnection =
    runtime::SubscriptionConnection<crate::excel_rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionServerHandle =
    server::SubscriptionServerHandle<crate::excel_rtd::RtdSubscriptionHost>;

#[cfg(all(feature = "rtd", feature = "bench-internals"))]
pub use channel::channel_protocol_probe;
#[cfg(feature = "rtd")]
pub use channel::{RtdChannelSource, RtdChannelSubscription, RtdSender};
#[cfg(feature = "rtd")]
pub use source::{RtdSink, RtdSource, RtdSourceHandle, RtdSubscription};
#[cfg(feature = "rtd")]
pub use topic::{RtdCapacity, RtdLimits};
#[cfg(feature = "rtd")]
pub use topic::{RtdTopic, RtdTopicParts};
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
pub(crate) use server::{PANIC_AFTER_TERMINATION_GUARD, TerminationAdmission};
#[cfg(feature = "rtd")]
pub(crate) use source::{SourceArena, SourceRegistration};
#[cfg(feature = "rtd")]
pub(crate) use topic::BorrowedTopicParts;
#[cfg(any(
    all(test, feature = "rtd"),
    all(feature = "bench-internals", feature = "rtd")
))]
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
