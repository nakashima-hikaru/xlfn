#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test module"
    )
)]
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
mod data_plane;
mod delivery;
mod host;
mod identity;
mod runtime;
mod runtime_services;
mod server;
mod source;
mod topic;
mod value;

pub(crate) type ErasedSink = delivery::ErasedSink;
pub(crate) type SubscriptionRuntime =
    runtime::SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionConnection =
    runtime::SubscriptionConnection<crate::excel_rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionServerHandle =
    server::SubscriptionServerHandle<crate::excel_rtd::RtdSubscriptionHost>;

#[cfg(feature = "rtd")]
pub use source::{
    RtdCancellation, RtdCancellationHandle, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription,
};
#[cfg(feature = "rtd")]
pub use topic::RtdTopic;
#[cfg(feature = "rtd")]
pub use topic::{RtdCapacity, RtdLimits};
#[cfg(feature = "rtd")]
pub use value::IntoRtdValue;
#[cfg(feature = "rtd")]
pub use value::RtdValue;
#[cfg(not(feature = "rtd"))]
pub(crate) use value::RtdValue;

#[cfg(any(test, all(feature = "bench-internals", feature = "rtd")))]
pub(crate) use crate::generation::ServerGeneration;
#[cfg(test)]
pub(crate) use crate::generation::{ConnectionGeneration, RuntimeGeneration};
#[cfg(test)]
use crate::value::ExcelErrorValue;
#[cfg(test)]
use crate::{XllError, XllResult};
#[cfg(test)]
pub(crate) use catalog::SubscriptionCatalog;
#[cfg(test)]
pub(crate) use data_plane::{
    OwnedPublishOperation, PublishCore, RtdRefreshBatch, ScopedPublishOperation,
};
#[cfg(all(target_os = "windows", feature = "rtd"))]
pub(crate) use delivery::RtdUpdate;
#[cfg(test)]
pub(crate) use delivery::{
    ActiveSubscription, DeliveryPhase, NotificationAttempt, NotificationCompletion,
    PreparedNotification, QueuedUpdate, RefreshState, SERVER_LIFECYCLE_CLOSING,
    SERVER_LIFECYCLE_OPEN, SERVER_LIFECYCLE_TERMINATED, SignalState, TopicShard, shard_index,
};
#[cfg(any(test, all(feature = "bench-internals", feature = "rtd")))]
pub(crate) use delivery::{RefreshOutcome, TOPIC_SHARDS};
pub(crate) use host::SubscriptionHost;
#[cfg(test)]
pub(crate) use identity::{NEXT_RTD_RUNTIME_ID, SubscriptionIdentityIndex, allocate_runtime_id};
#[cfg(test)]
use parking_lot::{Condvar, Mutex};
#[cfg(test)]
pub(crate) use runtime::OperationEnterHook;
pub(crate) use runtime_services::RuntimeServices;
#[cfg(test)]
pub(crate) use server::{
    OwnedServerOperation, PANIC_AFTER_TERMINATION_GUARD, ServerTermination, ServerTerminationPhase,
    ServerTerminationWaiter, SubscriptionServer, TerminationAdmission, TerminationCompletionGuard,
    TerminationCoordinator, TerminationState, cleanup_catalog_binding_and_pending,
    disconnect_all_no_unwind, disconnect_one_no_unwind, drop_notifier_no_unwind,
};
#[cfg(feature = "rtd")]
pub(crate) use source::SourceHandleAllocator;
#[cfg(test)]
pub(crate) use source::{ErasedRtdSource, SourceHandleId};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::marker::PhantomData;
#[cfg(test)]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::{Arc, Weak};
pub(crate) use topic::SubscriptionKey;
#[cfg(any(test, all(feature = "bench-internals", feature = "rtd")))]
pub(crate) use topic::TopicId;
#[cfg(test)]
pub(crate) use topic::{
    DEFAULT_MAX_RTD_ACTIVE, DEFAULT_MAX_RTD_PENDING, DEFAULT_MAX_RTD_QUEUED_UPDATES,
    DEFAULT_MAX_RTD_SOURCE_IDS, DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES, MAX_RTD_TOPIC_BYTES,
    MAX_RTD_TOPIC_PARTS, SourceId, SubscriptionId, SubscriptionIdentity,
};
#[cfg(any(all(target_os = "windows", feature = "rtd"), test))]
pub(crate) use value::StoredRtdValue;
#[cfg(all(test, feature = "rtd"))]
pub(crate) mod tests;
