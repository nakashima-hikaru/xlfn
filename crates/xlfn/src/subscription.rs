#![cfg_attr(
    test,
    allow(
        unused_imports,
        reason = "Test-only protocol fixtures are shared with the child test module"
    )
)]
#![cfg_attr(
    not(target_os = "windows"),
    allow(dead_code, reason = "Internal helpers for Windows COM integration")
)]

mod catalog;
mod delivery;
mod host;
mod identity;
mod runtime;
mod server;
mod source;
mod topic;
mod value;

pub(crate) type ErasedSink = delivery::ErasedSink;
pub(crate) type SubscriptionRuntime = runtime::SubscriptionRuntime<crate::rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionConnection =
    runtime::SubscriptionConnection<crate::rtd::RtdSubscriptionHost>;
pub(crate) type SubscriptionServerHandle =
    server::SubscriptionServerHandle<crate::rtd::RtdSubscriptionHost>;

pub use source::{
    RtdCancellation, RtdCancellationHandle, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription,
};
pub use topic::{RtdLimits, RtdTopic};
pub use value::{IntoRtdValue, RtdValue};

#[cfg(any(target_os = "windows", test, feature = "bench-internals"))]
pub(crate) use crate::generation::ServerGeneration;
#[cfg(test)]
pub(crate) use crate::generation::{ConnectionGeneration, RuntimeGeneration};
#[cfg(test)]
use crate::value::ExcelErrorValue;
#[cfg(test)]
use crate::{XllError, XllResult};
#[cfg(test)]
pub(crate) use catalog::{SubscriptionCatalog, SubscriptionState};
#[cfg(any(target_os = "windows", test, feature = "bench-internals"))]
pub(crate) use delivery::RefreshOutcome;
#[cfg(target_os = "windows")]
pub(crate) use delivery::RtdUpdate;
#[cfg(test)]
pub(crate) use delivery::{
    ActiveSubscription, DeliveryPhase, NotificationAttempt, NotificationCompletion,
    PreparedNotification, QueuedUpdate, RefreshState, SERVER_LIFECYCLE_CLOSING,
    SERVER_LIFECYCLE_OPEN, SERVER_LIFECYCLE_TERMINATED, SignalState, TOPIC_SHARDS, TopicShard,
    shard_index,
};
pub(crate) use host::SubscriptionHost;
#[cfg(test)]
pub(crate) use identity::{
    NEXT_RTD_RUNTIME_ID, SourceIdentityRegistry, SourceIdentityReservation,
    SubscriptionIdentityIndex, allocate_runtime_id,
};
#[cfg(test)]
use parking_lot::{Condvar, Mutex};
#[cfg(test)]
pub(crate) use runtime::OperationEnterHook;
#[cfg(test)]
pub(crate) use server::{
    OwnedServerOperation, PANIC_AFTER_TERMINATION_GUARD, PublishCore, RtdRefreshBatch,
    ScopedServerOperation, ServerReservationFailure, ServerTermination, ServerTerminationPhase,
    ServerTerminationWaiter, SubscriptionServer, TerminatedTopic, TerminationAdmission,
    TerminationCompletionGuard, TerminationCoordinator, TerminationState,
    cleanup_catalog_binding_and_pending, disconnect_all_no_unwind, disconnect_one_no_unwind,
    drop_notifier_no_unwind,
};
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
#[cfg(any(target_os = "windows", test, feature = "bench-internals"))]
pub(crate) use topic::TopicId;
#[cfg(test)]
pub(crate) use topic::{
    DEFAULT_MAX_RTD_ACTIVE, DEFAULT_MAX_RTD_PENDING, DEFAULT_MAX_RTD_QUEUED_UPDATES,
    DEFAULT_MAX_RTD_SOURCE_IDS, DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES, MAX_RTD_TOPIC_BYTES,
    MAX_RTD_TOPIC_PARTS, SourceId, SubscriptionIdentity,
};
#[cfg(any(target_os = "windows", test))]
pub(crate) use value::StoredRtdValue;
#[cfg(test)]
pub(crate) mod tests;
