#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::{ExcelErrorValue, XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RtdTopic {
    parts: Arc<[String]>,
}

impl RtdTopic {
    pub fn new(parts: impl IntoIterator<Item = impl Into<String>>) -> XllResult<Self> {
        let parts = parts.into_iter().map(Into::into).collect::<Vec<_>>();
        if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::Malformed("RTD topics require non-empty parts"),
            ));
        }
        if parts
            .iter()
            .any(|part| part.encode_utf16().count() > 32_767)
        {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::TooLarge {
                    limit: 32_767,
                    actual: parts
                        .iter()
                        .map(|part| part.encode_utf16().count())
                        .max()
                        .unwrap_or(0),
                },
            ));
        }
        Ok(Self {
            parts: Arc::from(parts),
        })
    }

    pub fn single(part: impl Into<String>) -> XllResult<Self> {
        Self::new([part.into()])
    }

    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
}

/// Defines the contract for an active RTD subscription.
///
/// # Safety
/// Implementations MUST guarantee that after `disconnect_and_wait` returns,
/// no background thread, worker, or native callback retains any reference to
/// or can execute any code within the module.
pub unsafe trait RtdSubscription: Send + 'static {
    /// Requests cooperative cancellation without waiting for background work.
    ///
    /// This method must be non-blocking, idempotent, must not panic, and must
    /// not call Excel or re-enter RTD lifecycle operations.
    fn request_cancel(&self);

    /// Releases the subscription and waits until its callbacks and native work
    /// can no longer execute add-in code.
    ///
    /// Implementations must honor [`Self::request_cancel`], avoid Excel/RTD
    /// lifecycle reentry, and return only after quiescence. A vendor operation
    /// that cannot be interrupted should be isolated out of process rather
    /// than allowed to block XLL unload.
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

/// A scalar value supported by Excel's RTD COM transport.
#[derive(Clone, Debug, PartialEq)]
pub enum RtdValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(String),
    Error(ExcelErrorValue),
    Empty,
}

impl RtdValue {
    fn validate(&self) -> XllResult<()> {
        match self {
            Self::Number(value) if !value.is_finite() => Err(XllError::Domain {
                code: crate::DomainErrorCode::InvalidInput,
            }),
            Self::String(value) => {
                crate::utf16::encode_bounded(value, "RTD value", crate::utf16::EXCEL_STRING_LIMIT)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl TryFrom<crate::OwnedExcelValue> for RtdValue {
    type Error = XllError;

    fn try_from(value: crate::OwnedExcelValue) -> XllResult<Self> {
        let value = match value {
            crate::OwnedExcelValue::Number(value) => Self::Number(value),
            crate::OwnedExcelValue::Boolean(value) => Self::Boolean(value),
            crate::OwnedExcelValue::Integer(value) => Self::Integer(value),
            crate::OwnedExcelValue::String(value) => Self::String(value),
            crate::OwnedExcelValue::Error(value) => Self::Error(value),
            crate::OwnedExcelValue::Missing | crate::OwnedExcelValue::Blank => Self::Empty,
            crate::OwnedExcelValue::Matrix(_) | crate::OwnedExcelValue::ArrayOutput(_) => {
                return Err(XllError::input(
                    "RTD value",
                    crate::InputError::Malformed("RTD values must be scalar"),
                ));
            }
        };
        value.validate()?;
        Ok(value)
    }
}

impl crate::IntoExcelValue for RtdValue {
    fn into_excel_value(self) -> XllResult<crate::OwnedExcelValue> {
        self.validate()?;
        Ok(match self {
            Self::Number(value) => crate::OwnedExcelValue::Number(value),
            Self::Boolean(value) => crate::OwnedExcelValue::Boolean(value),
            Self::Integer(value) => crate::OwnedExcelValue::Integer(value),
            Self::String(value) => crate::OwnedExcelValue::String(value),
            Self::Error(value) => crate::OwnedExcelValue::Error(value),
            Self::Empty => crate::OwnedExcelValue::Blank,
        })
    }
}

impl crate::ExcelReturn for RtdValue {
    type Output = Self;

    fn into_excel(self, _: &mut crate::ReturnContext<'_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

/// Converts a value into the scalar representation accepted by Excel RTD.
pub trait IntoRtdValue {
    fn into_rtd_value(self) -> XllResult<RtdValue>;
}

impl IntoRtdValue for RtdValue {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(self)
    }
}

impl IntoRtdValue for f64 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        if self.is_finite() {
            Ok(RtdValue::Number(self))
        } else {
            Err(XllError::Domain {
                code: crate::DomainErrorCode::InvalidInput,
            })
        }
    }
}

impl IntoRtdValue for bool {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Boolean(self))
    }
}

impl IntoRtdValue for i32 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Integer(self))
    }
}

impl IntoRtdValue for i64 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            Ok(RtdValue::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: crate::DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoRtdValue for crate::ExcelSerialDate {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        self.serial().into_rtd_value()
    }
}

impl IntoRtdValue for String {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::String(self))
    }
}

impl IntoRtdValue for &str {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::String(self.to_owned()))
    }
}

impl IntoRtdValue for ExcelErrorValue {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Error(self))
    }
}

impl IntoRtdValue for () {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Empty)
    }
}

pub trait RtdSource: Send + Sync + 'static {
    type Value: IntoRtdValue + Send + 'static;

    /// Creates a subscription without performing unbounded blocking work.
    ///
    /// Long-running work should begin asynchronously and be owned by the
    /// returned [`RtdSubscription`], whose cancellation contract makes XLL
    /// shutdown deterministic.
    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>>;
}

pub struct RtdSink<T> {
    sink: ErasedSink,
    _value: PhantomData<fn(T)>,
}

impl<T> Clone for RtdSink<T> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> RtdSink<T>
where
    T: IntoRtdValue,
{
    pub fn publish(&self, value: T) -> XllResult<()> {
        let value = value.into_rtd_value()?;
        value.validate()?;
        self.sink.publish(value)
    }
}

trait ErasedRtdSource: Send + Sync {
    fn subscribe(&self, topic: &RtdTopic, sink: ErasedSink) -> XllResult<Box<dyn RtdSubscription>>;
}

struct SourceAdapter<S>(Arc<S>);

impl<S> ErasedRtdSource for SourceAdapter<S>
where
    S: RtdSource,
{
    fn subscribe(&self, topic: &RtdTopic, sink: ErasedSink) -> XllResult<Box<dyn RtdSubscription>> {
        self.0.subscribe(
            topic,
            RtdSink {
                sink,
                _value: PhantomData,
            },
        )
    }
}

struct PendingSubscription {
    preparation_id: u64,
    live_reservations: usize,
    committed: bool,
    source: Arc<dyn ErasedRtdSource>,
    topic: RtdTopic,
    server_generation: Option<u64>,
    connecting_generation: Option<u64>,
}

struct ActiveSubscription {
    key: String,
    generation: u64,
    subscription: Option<Box<dyn RtdSubscription>>,
    committed: bool,
    latest: RtdValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparationOwnership {
    CreatedPending,
    ExistingPending,
    ExistingActive,
}

pub(crate) struct PreparedSubscription {
    runtime: Weak<SubscriptionRuntime>,
    key: String,
    reservation_id: Option<u64>,
    ownership: PreparationOwnership,
}

impl PreparedSubscription {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    #[cfg(test)]
    fn ownership(&self) -> PreparationOwnership {
        self.ownership
    }

    pub(crate) fn commit(mut self) {
        self.finish(true);
    }

    pub(crate) fn rollback(mut self) {
        self.finish(false);
    }

    fn finish(&mut self, committed: bool) {
        if self.ownership == PreparationOwnership::ExistingActive {
            debug_assert!(self.reservation_id.is_none());
            return;
        }
        let Some(reservation_id) = self.reservation_id.take() else {
            return;
        };
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.finish_preparation(&self.key, reservation_id, committed);
        }
    }
}

impl Drop for PreparedSubscription {
    fn drop(&mut self) {
        self.finish(false);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct TopicOwner {
    server_generation: u64,
    topic_id: i32,
}

pub(crate) struct SubscriptionConnection {
    runtime: Weak<SubscriptionRuntime>,
    owner: TopicOwner,
    generation: u64,
    key: String,
    value: RtdValue,
    created: bool,
    finished: bool,
}

impl SubscriptionConnection {
    pub(crate) fn value(&self) -> &RtdValue {
        &self.value
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        if self.created {
            let runtime = self.runtime.upgrade().ok_or(XllError::Closing)?;
            runtime.commit_connection(self.owner, self.generation, &self.key)?;
        }
        self.finished = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created
            && let Some(runtime) = self.runtime.upgrade()
        {
            runtime.rollback_connection(self.owner, self.generation, &self.key);
        }
    }
}

impl Drop for SubscriptionConnection {
    fn drop(&mut self) {
        self.rollback();
    }
}

struct QueuedUpdate {
    sequence: u64,
    value: RtdValue,
}

pub(crate) struct RtdUpdate {
    owner: TopicOwner,
    sequence: u64,
    pub(crate) topic_id: i32,
    pub(crate) value: RtdValue,
}

#[cfg(test)]
impl RtdUpdate {
    pub(crate) fn for_test(topic_id: i32, value: RtdValue) -> Self {
        Self {
            owner: TopicOwner {
                server_generation: 0,
                topic_id,
            },
            sequence: 0,
            topic_id,
            value,
        }
    }
}

pub(crate) struct RtdUpdateBatch {
    pub(crate) updates: Vec<RtdUpdate>,
}

struct SourceIdentity {
    id: u64,
    source: Weak<dyn Any + Send + Sync>,
}

struct SubscriptionState {
    closed: bool,
    in_flight: usize,
    in_flight_by_server: HashMap<u64, usize>,
    terminating_servers: HashSet<u64>,
    terminated_servers: HashSet<u64>,
    pending: HashMap<String, PendingSubscription>,
    active: HashMap<TopicOwner, ActiveSubscription>,
    topic_ids: HashMap<String, TopicOwner>,
    updates: BTreeMap<TopicOwner, QueuedUpdate>,
    next_update_sequence: u64,
    source_ids: HashMap<usize, SourceIdentity>,
}

#[allow(clippy::type_complexity)]
pub(crate) struct SubscriptionRuntime {
    state: Mutex<SubscriptionState>,
    idle: Condvar,
    cleanup_failure: Mutex<Option<XllError>>,
    notifications: RwLock<HashMap<u64, Arc<dyn Fn() -> XllResult<()> + Send + Sync>>>,
    next_preparation_id: AtomicU64,
    next_generation: AtomicU64,
}

pub(crate) struct SubscriptionOperation<'a> {
    runtime: &'a SubscriptionRuntime,
    server_generation: Option<u64>,
}

impl Drop for SubscriptionOperation<'_> {
    fn drop(&mut self) {
        let mut state = self.runtime.state.lock();
        state.in_flight = state
            .in_flight
            .checked_sub(1)
            .expect("RTD operation count remains balanced");
        if let Some(server_generation) = self.server_generation {
            let remove = {
                let count = state
                    .in_flight_by_server
                    .get_mut(&server_generation)
                    .expect("RTD server operation count remains installed");
                *count = count
                    .checked_sub(1)
                    .expect("RTD server operation count remains balanced");
                *count == 0
            };
            if remove {
                state.in_flight_by_server.remove(&server_generation);
            }
        }
        self.runtime.idle.notify_all();
    }
}

impl SubscriptionRuntime {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(SubscriptionState {
                closed: false,
                in_flight: 0,
                in_flight_by_server: HashMap::new(),
                terminating_servers: HashSet::new(),
                terminated_servers: HashSet::new(),
                pending: HashMap::new(),
                active: HashMap::new(),
                topic_ids: HashMap::new(),
                updates: BTreeMap::new(),
                next_update_sequence: 1,
                source_ids: HashMap::new(),
            }),
            idle: Condvar::new(),
            cleanup_failure: Mutex::new(None),
            notifications: RwLock::new(HashMap::new()),
            next_preparation_id: AtomicU64::new(1),
            next_generation: AtomicU64::new(1),
        }
    }

    fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            let mut failure = self.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error);
            }
        }
    }

    fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup_failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    fn enter_operation(
        &self,
        server_generation: Option<u64>,
    ) -> XllResult<SubscriptionOperation<'_>> {
        let mut state = self.state.lock();
        self.enter_operation_locked(&mut state, server_generation)
    }

    fn enter_operation_locked<'a>(
        &'a self,
        state: &mut SubscriptionState,
        server_generation: Option<u64>,
    ) -> XllResult<SubscriptionOperation<'a>> {
        if state.closed
            || server_generation.is_some_and(|generation| {
                state.terminating_servers.contains(&generation)
                    || state.terminated_servers.contains(&generation)
            })
        {
            return Err(XllError::Closing);
        }
        let next_in_flight = state.in_flight.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5254_444f_5043_4e54,
        })?;
        let next_server_count = server_generation
            .map(|generation| {
                state
                    .in_flight_by_server
                    .get(&generation)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(XllError::Internal {
                        diagnostic_id: 0x5254_4453_5256_434e,
                    })
            })
            .transpose()?;
        state.in_flight = next_in_flight;
        if let Some(server_generation) = server_generation {
            state.in_flight_by_server.insert(
                server_generation,
                next_server_count.expect("server count was computed above"),
            );
        }
        Ok(SubscriptionOperation {
            runtime: self,
            server_generation,
        })
    }

    pub(crate) fn enter_server_operation(
        &self,
        server_generation: u64,
    ) -> XllResult<SubscriptionOperation<'_>> {
        self.enter_operation(Some(server_generation))
    }

    pub(crate) fn prepare<S>(
        self: &Arc<Self>,
        source: Arc<S>,
        topic: RtdTopic,
    ) -> XllResult<PreparedSubscription>
    where
        S: RtdSource,
    {
        static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(1);
        let ptr_key = Arc::as_ptr(&source) as usize;
        let erased_source: Arc<dyn Any + Send + Sync> = source.clone();
        let mut state = self.state.lock();
        if state.closed {
            return Err(XllError::Closing);
        }
        state
            .source_ids
            .retain(|_, identity| identity.source.strong_count() != 0);
        let source_identity = match state.source_ids.get(&ptr_key) {
            Some(identity)
                if identity
                    .source
                    .upgrade()
                    .is_some_and(|existing| Arc::ptr_eq(&existing, &erased_source)) =>
            {
                identity.id
            }
            _ => {
                let id = NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed);
                state.source_ids.insert(
                    ptr_key,
                    SourceIdentity {
                        id,
                        source: Arc::downgrade(&erased_source),
                    },
                );
                id
            }
        };

        let mut hash = blake3::Hasher::new();
        hash.update(&source_identity.to_le_bytes());
        for part in topic.parts() {
            hash.update(&(part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        let key = format!("stream:{}", hash.finalize().to_hex());
        if let Some(pending) = state.pending.get_mut(&key) {
            pending.live_reservations =
                pending
                    .live_reservations
                    .checked_add(1)
                    .ok_or(XllError::Internal {
                        diagnostic_id: 0x5254_4452_534c_4541,
                    })?;
            return Ok(PreparedSubscription {
                runtime: Arc::downgrade(self),
                key,
                reservation_id: Some(pending.preparation_id),
                ownership: PreparationOwnership::ExistingPending,
            });
        }
        if let Some(owner) = state.topic_ids.get(&key) {
            state.active.get(owner).ok_or(XllError::Internal {
                diagnostic_id: 0x5254_4450_5245_5041,
            })?;
            return Ok(PreparedSubscription {
                runtime: Arc::downgrade(self),
                key,
                reservation_id: None,
                ownership: PreparationOwnership::ExistingActive,
            });
        }

        let preparation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);
        state.pending.insert(
            key.clone(),
            PendingSubscription {
                preparation_id,
                live_reservations: 1,
                committed: false,
                source: Arc::new(SourceAdapter(source)),
                topic,
                server_generation: None,
                connecting_generation: None,
            },
        );
        Ok(PreparedSubscription {
            runtime: Arc::downgrade(self),
            key,
            reservation_id: Some(preparation_id),
            ownership: PreparationOwnership::CreatedPending,
        })
    }

    #[cfg(test)]
    pub(crate) fn connect(
        self: &Arc<Self>,
        server_generation: u64,
        topic_id: i32,
        key: &str,
    ) -> XllResult<RtdValue> {
        let connection = self.connect_transaction(server_generation, topic_id, key)?;
        let value = connection.value().clone();
        connection.commit()?;
        Ok(value)
    }

    pub(crate) fn connect_transaction(
        self: &Arc<Self>,
        server_generation: u64,
        topic_id: i32,
        key: &str,
    ) -> XllResult<SubscriptionConnection> {
        let _operation = self.enter_operation(Some(server_generation))?;
        let owner = TopicOwner {
            server_generation,
            topic_id,
        };
        let (source, topic, generation, sink) = {
            let mut state = self.state.lock();
            if state.closed {
                return Err(XllError::Closing);
            }
            if let Some(active) = state.active.get(&owner) {
                return if active.key == key && active.committed && active.subscription.is_some() {
                    Ok(SubscriptionConnection {
                        runtime: Arc::downgrade(self),
                        owner,
                        generation: active.generation,
                        key: key.to_owned(),
                        value: active.latest.clone(),
                        created: false,
                        finished: false,
                    })
                } else if active.key == key {
                    Err(XllError::Overloaded)
                } else {
                    Err(XllError::InvalidHandle)
                };
            }
            if state.topic_ids.contains_key(key) {
                return Err(XllError::InvalidHandle);
            }
            let pending = state.pending.get_mut(key).ok_or(XllError::InvalidHandle)?;
            if pending
                .server_generation
                .is_some_and(|existing| existing != server_generation)
            {
                return Err(XllError::InvalidHandle);
            }
            if pending.connecting_generation.is_some() {
                return Err(XllError::InvalidHandle);
            }
            let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
            pending.connecting_generation = Some(generation);
            let source = Arc::clone(&pending.source);
            let topic = pending.topic.clone();
            let initial = RtdValue::Error(ExcelErrorValue(crate::ExcelError::NotAvailable));
            state.active.insert(
                owner,
                ActiveSubscription {
                    key: key.to_owned(),
                    generation,
                    subscription: None,
                    committed: false,
                    latest: initial,
                },
            );
            state.topic_ids.insert(key.to_owned(), owner);
            let sink = ErasedSink {
                runtime: Arc::downgrade(self),
                owner,
                generation,
            };
            (source, topic, generation, sink)
        };

        let subscription = match catch_unwind(AssertUnwindSafe(|| source.subscribe(&topic, sink))) {
            Ok(Ok(subscription)) => subscription,
            Ok(Err(error)) => {
                self.finish_failed_connection(owner, generation, key);
                self.record_cleanup_result(drop_erased_source_no_unwind(
                    source,
                    "rtd_connect_source_drop",
                ));
                return Err(error);
            }
            Err(_) => {
                self.finish_failed_connection(owner, generation, key);
                self.record_cleanup_result(drop_erased_source_no_unwind(
                    source,
                    "rtd_connect_source_drop",
                ));
                return Err(XllError::Panic);
            }
        };

        let mut subscription = Some(subscription);
        let (latest, retired_pending) = {
            let mut state = self.state.lock();
            let can_install = !state.closed
                && !state.terminating_servers.contains(&server_generation)
                && !state.terminated_servers.contains(&server_generation);
            let installed = match state.active.get_mut(&owner) {
                Some(active) if can_install && active.generation == generation => {
                    active.subscription = subscription.take();
                    Some(active.latest.clone())
                }
                _ => None,
            };
            if let Some(latest) = installed {
                (Some(latest), None)
            } else {
                Self::remove_active_attempt(&mut state, owner, generation, key);
                let retired_pending = Self::reset_pending_connection(&mut state, key, generation);
                (None, retired_pending)
            }
        };
        // Source ownership and disconnect may execute user code.
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            retired_pending,
            "rtd_connected_pending_source_drop",
        ));
        self.record_cleanup_result(drop_erased_source_no_unwind(
            source,
            "rtd_connect_source_drop",
        ));
        if let Some(subscription) = subscription {
            self.record_cleanup_result(disconnect_one_no_unwind(subscription, owner, key));
        }
        match latest {
            Some(value) => Ok(SubscriptionConnection {
                runtime: Arc::downgrade(self),
                owner,
                generation,
                key: key.to_owned(),
                value,
                created: true,
                finished: false,
            }),
            None => Err(XllError::Closing),
        }
    }

    fn commit_connection(&self, owner: TopicOwner, generation: u64, key: &str) -> XllResult<()> {
        let _operation = self.enter_operation(Some(owner.server_generation))?;
        let retired_pending = {
            let mut state = self.state.lock();
            if state.closed
                || state.terminating_servers.contains(&owner.server_generation)
                || state.terminated_servers.contains(&owner.server_generation)
            {
                return Err(XllError::Closing);
            }
            let pending_matches = state
                .pending
                .get(key)
                .is_some_and(|pending| pending.connecting_generation == Some(generation));
            if !pending_matches {
                return Err(XllError::Internal {
                    diagnostic_id: 0x5254_4443_4f4d_4d49,
                });
            }
            let active = state.active.get_mut(&owner).ok_or(XllError::Internal {
                diagnostic_id: 0x5254_4443_4f4d_4143,
            })?;
            if active.generation != generation || active.key != key || active.subscription.is_none()
            {
                return Err(XllError::Internal {
                    diagnostic_id: 0x5254_4443_4f4d_4d54,
                });
            }
            active.committed = true;
            state.pending.remove(key)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            retired_pending,
            "rtd_committed_pending_source_drop",
        ));
        Ok(())
    }

    fn rollback_connection(&self, owner: TopicOwner, generation: u64, key: &str) {
        let Ok(_operation) = self.enter_operation(Some(owner.server_generation)) else {
            return;
        };
        let (subscription, retired_pending) = {
            let mut state = self.state.lock();
            let subscription = if state.active.get(&owner).is_some_and(|active| {
                active.generation == generation && active.key == key && !active.committed
            }) {
                let active = state
                    .active
                    .remove(&owner)
                    .expect("the uncommitted RTD connection was checked above");
                if state.topic_ids.get(key) == Some(&owner) {
                    state.topic_ids.remove(key);
                }
                state.updates.remove(&owner);
                active.subscription
            } else {
                None
            };
            let retired_pending = Self::reset_pending_connection(&mut state, key, generation);
            (subscription, retired_pending)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            retired_pending,
            "rtd_connection_rollback_source_drop",
        ));
        if let Some(subscription) = subscription {
            self.record_cleanup_result(disconnect_one_no_unwind(subscription, owner, key));
        }
    }

    fn finish_preparation(&self, key: &str, reservation_id: u64, committed: bool) {
        let removed = {
            let mut state = self.state.lock();
            let Some(pending) = state.pending.get_mut(key) else {
                return;
            };
            if pending.preparation_id != reservation_id {
                return;
            }
            pending.live_reservations = pending
                .live_reservations
                .checked_sub(1)
                .expect("every RTD preparation lease is finished exactly once");
            pending.committed |= committed;
            let connecting = pending.connecting_generation.is_some();
            let unowned = pending.live_reservations == 0 && !pending.committed;
            let server_generation = pending.server_generation;
            let invalid_generation = state.closed
                || server_generation.is_some_and(|generation| {
                    state.terminating_servers.contains(&generation)
                        || state.terminated_servers.contains(&generation)
                });
            (!connecting && (unowned || invalid_generation))
                .then(|| state.pending.remove(key))
                .flatten()
        };
        // The source is user-owned and its Drop implementation may re-enter
        // runtime services. Release it only after the state lock is gone.
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            removed,
            "rtd_preparation_source_drop",
        ));
    }

    pub(crate) fn claim_server(&self, key: &str, server_generation: u64) -> XllResult<()> {
        let mut state = self.state.lock();
        if state.closed
            || state.terminating_servers.contains(&server_generation)
            || state.terminated_servers.contains(&server_generation)
        {
            return Err(XllError::Closing);
        }
        if let Some(pending) = state.pending.get_mut(key) {
            if pending
                .server_generation
                .is_some_and(|existing| existing != server_generation)
            {
                return Err(XllError::InvalidHandle);
            }
            pending.server_generation = Some(server_generation);
            return Ok(());
        }
        if state
            .topic_ids
            .get(key)
            .is_some_and(|owner| owner.server_generation == server_generation)
        {
            Ok(())
        } else {
            Err(XllError::InvalidHandle)
        }
    }

    fn finish_failed_connection(&self, owner: TopicOwner, generation: u64, key: &str) {
        let retired_pending = {
            let mut state = self.state.lock();
            Self::remove_active_attempt(&mut state, owner, generation, key);
            Self::reset_pending_connection(&mut state, key, generation)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            retired_pending,
            "rtd_failed_connection_source_drop",
        ));
    }

    fn remove_active_attempt(
        state: &mut SubscriptionState,
        owner: TopicOwner,
        generation: u64,
        key: &str,
    ) {
        if state
            .active
            .get(&owner)
            .is_some_and(|active| active.generation == generation && active.key == key)
        {
            state.active.remove(&owner);
            if state.topic_ids.get(key) == Some(&owner) {
                state.topic_ids.remove(key);
            }
            state.updates.remove(&owner);
        }
    }

    fn reset_pending_connection(
        state: &mut SubscriptionState,
        key: &str,
        generation: u64,
    ) -> Option<PendingSubscription> {
        let pending = state.pending.get_mut(key)?;
        if pending.connecting_generation != Some(generation) {
            return None;
        }
        pending.connecting_generation = None;
        let unowned = pending.live_reservations == 0 && !pending.committed;
        let server_generation = pending.server_generation;
        let should_remove = unowned
            || state.closed
            || server_generation.is_some_and(|generation| {
                state.terminating_servers.contains(&generation)
                    || state.terminated_servers.contains(&generation)
            });
        should_remove.then(|| state.pending.remove(key)).flatten()
    }

    pub(crate) fn disconnect(&self, server_generation: u64, topic_id: i32) {
        let Ok(_operation) = self.enter_operation(Some(server_generation)) else {
            return;
        };
        let owner = TopicOwner {
            server_generation,
            topic_id,
        };
        let subscription = {
            let mut state = self.state.lock();
            let Some(active) = state.active.remove(&owner) else {
                return;
            };
            state.topic_ids.remove(&active.key);
            state.updates.remove(&owner);
            active
                .subscription
                .map(|subscription| (active.key, subscription))
        };
        if let Some((key, subscription)) = subscription {
            self.record_cleanup_result(disconnect_one_no_unwind(subscription, owner, &key));
        }
    }

    pub(crate) fn snapshot_updates(&self, server_generation: u64) -> RtdUpdateBatch {
        let state = self.state.lock();
        RtdUpdateBatch {
            updates: state
                .updates
                .iter()
                .filter(|&(owner, _queued)| owner.server_generation == server_generation)
                .map(|(owner, queued)| RtdUpdate {
                    owner: *owner,
                    sequence: queued.sequence,
                    topic_id: owner.topic_id,
                    value: queued.value.clone(),
                })
                .collect(),
        }
    }

    pub(crate) fn commit_updates(&self, batch: &RtdUpdateBatch) {
        let mut state = self.state.lock();
        for update in &batch.updates {
            if state
                .updates
                .get(&update.owner)
                .is_some_and(|queued| queued.sequence == update.sequence)
            {
                state.updates.remove(&update.owner);
            }
        }
    }

    pub(crate) fn has_pending_updates(&self, server_generation: u64) -> bool {
        let state = self.state.lock();
        state
            .updates
            .keys()
            .any(|owner| owner.server_generation == server_generation)
    }

    pub(crate) fn retry_updates(&self, server_generation: u64) {
        let Ok(_operation) = self.enter_operation(Some(server_generation)) else {
            return;
        };
        if self.has_pending_updates(server_generation) {
            self.notify_with_retry_inner(server_generation);
        }
    }

    pub(crate) fn set_notification(
        &self,
        server_generation: u64,
        notification: Option<Arc<dyn Fn() -> XllResult<()> + Send + Sync>>,
    ) {
        let retired = {
            let state = self.state.lock();
            if state.closed
                || state.terminating_servers.contains(&server_generation)
                || state.terminated_servers.contains(&server_generation)
            {
                drop(state);
                drop(notification);
                return;
            }
            let mut notifications = self.notifications.write();
            let retired = if let Some(notification) = notification {
                notifications.insert(server_generation, notification)
            } else {
                notifications.remove(&server_generation)
            };
            // Preserve the state -> notifications lock order while making
            // callback destruction fully lock-free.
            drop(notifications);
            drop(state);
            retired
        };
        drop(retired);
    }

    pub(crate) fn terminate_server(
        &self,
        server_generation: u64,
    ) -> XllResult<SubscriptionOperation<'_>> {
        let operation = self.enter_operation(None)?;
        {
            let mut state = self.state.lock();
            if state.terminated_servers.contains(&server_generation) {
                drop(state);
                self.cleanup_result()?;
                return Ok(operation);
            }
            if state.terminating_servers.contains(&server_generation) {
                // Waiting here can self-deadlock when external callback or
                // subscription teardown code re-enters termination. The first
                // caller owns the transition; concurrent attempts retry only
                // after it has reached the idempotent terminated state.
                return Err(XllError::Closing);
            }
            state.terminating_servers.insert(server_generation);
        }

        let retired_notification = {
            let mut notifications = self.notifications.write();
            notifications.remove(&server_generation)
        };
        drop(retired_notification);

        let mut subscriptions = {
            let mut state = self.state.lock();
            state
                .active
                .iter_mut()
                .filter_map(|(owner, active)| {
                    if owner.server_generation != server_generation {
                        return None;
                    }
                    active
                        .subscription
                        .take()
                        .map(|subscription| (*owner, active.key.clone(), subscription))
                })
                .collect::<Vec<_>>()
        };
        self.record_cleanup_result(request_cancel_all_no_unwind(&subscriptions));

        {
            let mut state = self.state.lock();
            while state
                .in_flight_by_server
                .get(&server_generation)
                .is_some_and(|count| *count != 0)
            {
                self.idle.wait(&mut state);
            }
        }

        let (late_subscriptions, removed_pending) = {
            let mut state = self.state.lock();
            let pending_keys = state
                .pending
                .iter()
                .filter(|(_, pending)| pending.server_generation == Some(server_generation))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            let removed_pending = pending_keys
                .into_iter()
                .filter_map(|key| state.pending.remove(&key))
                .collect::<Vec<_>>();
            let owners = state
                .active
                .keys()
                .filter(|owner| owner.server_generation == server_generation)
                .copied()
                .collect::<Vec<_>>();
            let late_subscriptions = owners
                .into_iter()
                .filter_map(|owner| {
                    state.updates.remove(&owner);
                    let active = state.active.remove(&owner)?;
                    state.topic_ids.remove(&active.key);
                    active
                        .subscription
                        .map(|subscription| (owner, active.key, subscription))
                })
                .collect::<Vec<_>>();
            (late_subscriptions, removed_pending)
        };
        // Pending sources are user-owned and may re-enter runtime services in
        // Drop. Never release them while the subscription state lock is held.
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            removed_pending,
            "rtd_termination_pending_source_drop",
        ));
        self.record_cleanup_result(request_cancel_all_no_unwind(&late_subscriptions));
        subscriptions.extend(late_subscriptions);
        self.record_cleanup_result(disconnect_all_no_unwind(subscriptions));

        let mut state = self.state.lock();
        state.terminating_servers.remove(&server_generation);
        state.terminated_servers.insert(server_generation);
        self.idle.notify_all();
        drop(state);
        self.cleanup_result()?;
        Ok(operation)
    }

    pub(crate) fn close(&self) -> XllResult<()> {
        let mut subscriptions = {
            let mut state = self.state.lock();
            state.closed = true;
            state
                .active
                .iter_mut()
                .filter_map(|(owner, active)| {
                    active
                        .subscription
                        .take()
                        .map(|subscription| (*owner, active.key.clone(), subscription))
                })
                .collect::<Vec<_>>()
        };
        let retired_notifications = {
            let mut notifications = self.notifications.write();
            std::mem::take(&mut *notifications)
        };
        drop(retired_notifications);
        self.record_cleanup_result(request_cancel_all_no_unwind(&subscriptions));

        let (late_subscriptions, removed_pending) = {
            let mut state = self.state.lock();
            while state.in_flight != 0 {
                self.idle.wait(&mut state);
            }
            let removed_pending = std::mem::take(&mut state.pending);
            state.topic_ids.clear();
            state.updates.clear();
            state.source_ids.clear();
            state.in_flight_by_server.clear();
            state.terminating_servers.clear();
            state.terminated_servers.clear();
            let late_subscriptions = state
                .active
                .drain()
                .filter_map(|(owner, active)| {
                    active
                        .subscription
                        .map(|subscription| (owner, active.key, subscription))
                })
                .collect::<Vec<_>>();
            (late_subscriptions, removed_pending)
        };
        // See terminate_server: source Drop is outside every runtime lock.
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            removed_pending.into_values(),
            "rtd_close_pending_source_drop",
        ));
        self.record_cleanup_result(request_cancel_all_no_unwind(&late_subscriptions));
        subscriptions.extend(late_subscriptions);
        self.record_cleanup_result(disconnect_all_no_unwind(subscriptions));
        self.cleanup_result()
    }

    fn publish(&self, owner: TopicOwner, generation: u64, value: RtdValue) -> XllResult<()> {
        let _operation = self.enter_operation(Some(owner.server_generation))?;
        {
            let mut state = self.state.lock();
            if state.closed {
                return Err(XllError::Closing);
            }
            if state
                .active
                .get(&owner)
                .filter(|active| active.generation == generation)
                .is_none()
            {
                return Err(XllError::Closing);
            }
            let sequence = state.next_update_sequence;
            state.next_update_sequence = sequence.checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: 0x5254_4455_5044_4154,
            })?;
            let active = state.active.get_mut(&owner).expect("active checked above");
            active.latest = value.clone();
            state
                .updates
                .insert(owner, QueuedUpdate { sequence, value });
        }
        self.notify_with_retry_inner(owner.server_generation);
        Ok(())
    }

    fn notify(&self, server_generation: u64) -> XllResult<()> {
        let notification = self.notifications.read().get(&server_generation).cloned();
        if let Some(notification) = notification {
            match catch_unwind(AssertUnwindSafe(|| notification())) {
                Ok(result) => result,
                Err(_) => Err(XllError::Panic),
            }
        } else {
            Ok(())
        }
    }

    fn notify_with_retry_inner(&self, server_generation: u64) {
        const MAX_RETRIES: usize = 3;
        let mut last_error = None;

        for attempt in 0..MAX_RETRIES {
            match self.notify(server_generation) {
                Ok(()) => return,
                Err(err) => {
                    last_error = Some(err);
                    if attempt + 1 < MAX_RETRIES {
                        std::thread::yield_now();
                    }
                }
            }
        }

        if let Some(err) = last_error {
            crate::diagnostics::report_no_unwind("rtd_update_notify", &err);
        }
    }
}

#[derive(Clone)]
struct ErasedSink {
    runtime: std::sync::Weak<SubscriptionRuntime>,
    owner: TopicOwner,
    generation: u64,
}

impl ErasedSink {
    fn publish(&self, value: RtdValue) -> XllResult<()> {
        self.runtime
            .upgrade()
            .ok_or(XllError::Closing)?
            .publish(self.owner, self.generation, value)
    }
}

type DisconnectTarget = (TopicOwner, String, Box<dyn RtdSubscription>);

fn report_source_drop_panic(operation: &'static str) {
    crate::diagnostics::report_no_unwind(operation, &XllError::Panic);
}

fn drop_erased_source_no_unwind(
    source: Arc<dyn ErasedRtdSource>,
    operation: &'static str,
) -> XllResult<()> {
    if catch_unwind(AssertUnwindSafe(|| drop(source))).is_err() {
        report_source_drop_panic(operation);
        Err(XllError::Panic)
    } else {
        Ok(())
    }
}

fn drop_pending_subscriptions_no_unwind(
    pending: impl IntoIterator<Item = PendingSubscription>,
    operation: &'static str,
) -> XllResult<()> {
    let mut failure = None;
    for pending in pending {
        if catch_unwind(AssertUnwindSafe(|| drop(pending))).is_err() {
            report_source_drop_panic(operation);
            if failure.is_none() {
                failure = Some(XllError::Panic);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn report_disconnect_failure(
    operation: &'static str,
    owner: TopicOwner,
    key: &str,
    error: &XllError,
) -> XllError {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::error!(
            server_generation = owner.server_generation,
            topic_id = owner.topic_id,
            subscription_key = key,
            error = %error,
            "RTD subscription shutdown failed"
        );
    }));
    let contextual = XllError::RtdSubscriptionShutdown {
        server_generation: owner.server_generation,
        topic_id: owner.topic_id,
        key: key.to_owned(),
        source: Box::new(error.clone()),
    };
    crate::diagnostics::report_no_unwind(operation, &contextual);
    contextual
}

fn request_cancel_no_unwind(
    subscription: &dyn RtdSubscription,
    owner: TopicOwner,
    key: &str,
) -> XllResult<()> {
    if catch_unwind(AssertUnwindSafe(|| subscription.request_cancel())).is_err() {
        Err(report_disconnect_failure(
            "rtd_subscription_request_cancel",
            owner,
            key,
            &XllError::Panic,
        ))
    } else {
        Ok(())
    }
}

fn disconnect_wait_no_unwind(
    subscription: Box<dyn RtdSubscription>,
    owner: TopicOwner,
    key: &str,
) -> XllResult<()> {
    match catch_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(report_disconnect_failure(
            "rtd_subscription_disconnect",
            owner,
            key,
            &error,
        )),
        Err(_) => Err(report_disconnect_failure(
            "rtd_subscription_disconnect",
            owner,
            key,
            &XllError::Panic,
        )),
    }
}

fn disconnect_one_no_unwind(
    subscription: Box<dyn RtdSubscription>,
    owner: TopicOwner,
    key: &str,
) -> XllResult<()> {
    let cancellation = request_cancel_no_unwind(subscription.as_ref(), owner, key);
    let disconnection = disconnect_wait_no_unwind(subscription, owner, key);
    cancellation.and(disconnection)
}

fn request_cancel_all_no_unwind(subscriptions: &[DisconnectTarget]) -> XllResult<()> {
    let mut failure = None;
    for (owner, key, subscription) in subscriptions {
        if let Err(error) = request_cancel_no_unwind(subscription.as_ref(), *owner, key)
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn disconnect_all_no_unwind(subscriptions: Vec<DisconnectTarget>) -> XllResult<()> {
    let mut failure = None;
    for (owner, key, subscription) in subscriptions {
        if let Err(error) = disconnect_wait_no_unwind(subscription, owner, &key)
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntoExcelValue;
    use static_assertions::assert_not_impl_any;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::{Duration, Instant};

    assert_not_impl_any!(crate::Matrix<f64>: IntoRtdValue);

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for concurrent RTD state"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn observed_rtd_values_preserve_the_scalar_contract() {
        assert_eq!(
            RtdValue::try_from(crate::OwnedExcelValue::Number(12.5)).unwrap(),
            RtdValue::Number(12.5)
        );
        assert_eq!(
            RtdValue::try_from(crate::OwnedExcelValue::Blank).unwrap(),
            RtdValue::Empty
        );
        assert_eq!(
            RtdValue::Empty.into_excel_value().unwrap(),
            crate::OwnedExcelValue::Blank
        );

        let matrix = crate::Matrix::new(1, 1, vec![crate::OwnedExcelValue::Number(1.0)]).unwrap();
        assert!(matches!(
            RtdValue::try_from(crate::OwnedExcelValue::Matrix(matrix)),
            Err(XllError::Input {
                argument: "RTD value",
                reason: crate::InputError::Malformed("RTD values must be scalar"),
            })
        ));
    }

    struct TestSubscription(Arc<AtomicBool>);

    // SAFETY: disconnect_and_wait ensures no background work accesses module code.
    unsafe impl RtdSubscription for TestSubscription {
        fn request_cancel(&self) {}

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.0.store(true, Ordering::Release);
            Ok(())
        }
    }

    struct TestSource {
        disconnected: Arc<AtomicBool>,
    }

    impl RtdSource for TestSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            sink.publish(12.5)?;
            Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
        }
    }

    struct ReentrantDropSource {
        runtime: Weak<SubscriptionRuntime>,
        dropped: mpsc::SyncSender<()>,
    }

    impl Drop for ReentrantDropSource {
        fn drop(&mut self) {
            if let Some(runtime) = self.runtime.upgrade() {
                runtime.set_notification(9_999, None);
            }
            self.dropped.send(()).unwrap();
        }
    }

    impl RtdSource for ReentrantDropSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            _sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            panic!("pending-only reentrant Drop source must not be subscribed")
        }
    }

    fn reentrant_drop_source(
        runtime: &Arc<SubscriptionRuntime>,
    ) -> (Arc<ReentrantDropSource>, mpsc::Receiver<()>) {
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        (
            Arc::new(ReentrantDropSource {
                runtime: Arc::downgrade(runtime),
                dropped: dropped_tx,
            }),
            dropped_rx,
        )
    }

    struct PanickingDropSource {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for PanickingDropSource {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
            panic!("injected RTD source drop panic");
        }
    }

    impl RtdSource for PanickingDropSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            sink.publish(17.0)?;
            Ok(Box::new(TestSubscription(Arc::new(AtomicBool::new(false)))))
        }
    }

    fn panicking_drop_source(dropped: &Arc<AtomicBool>) -> Arc<PanickingDropSource> {
        Arc::new(PanickingDropSource {
            dropped: Arc::clone(dropped),
        })
    }

    struct BlockingFailSource {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl RtdSource for BlockingFailSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            _sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            Err(XllError::Domain {
                code: crate::DomainErrorCode::NativeFailure,
            })
        }
    }

    #[test]
    fn source_lifecycle_and_updates_are_independent_from_handles() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let key = runtime
            .prepare(source, RtdTopic::single("EURUSD").unwrap())
            .unwrap();
        let initial = runtime.connect(1, 7, key.key()).unwrap();
        assert_eq!(initial, RtdValue::Number(12.5));
        let batch = runtime.snapshot_updates(1);
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].topic_id, 7);
        assert_eq!(batch.updates[0].value, RtdValue::Number(12.5));
        runtime.commit_updates(&batch);
        assert!(runtime.snapshot_updates(1).updates.is_empty());
        runtime.disconnect(1, 7);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn failed_observation_preserves_an_existing_active_subscription() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let topic = RtdTopic::single("rollback").unwrap();
        let created = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        runtime.connect(1, 8, created.key()).unwrap();
        let existing = runtime.prepare(source, topic).unwrap();
        assert_eq!(existing.ownership(), PreparationOwnership::ExistingActive);

        existing.rollback();

        let state = runtime.state.lock();
        assert_eq!(state.active.len(), 1);
        assert_eq!(state.updates.len(), 1);
        assert!(state.topic_ids.contains_key(created.key()));
        drop(state);
        assert!(!disconnected.load(Ordering::Acquire));

        runtime.disconnect(1, 8);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn failed_observation_preserves_an_existing_pending_subscription() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let source = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let topic = RtdTopic::single("shared-pending").unwrap();
        let created = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let existing = runtime.prepare(source, topic).unwrap();
        assert_eq!(created.ownership(), PreparationOwnership::CreatedPending);
        assert_eq!(existing.ownership(), PreparationOwnership::ExistingPending);

        existing.rollback();

        let state = runtime.state.lock();
        assert!(state.pending.contains_key(created.key()));
        assert!(state.active.is_empty());
        assert!(state.topic_ids.is_empty());
        drop(state);

        created.rollback();
        assert!(runtime.state.lock().pending.is_empty());
    }

    #[test]
    fn creator_failure_does_not_invalidate_a_later_pending_observer() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let topic = RtdTopic::single("creator-fails-first").unwrap();
        let creator = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let observer = runtime.prepare(source, topic).unwrap();
        let key = observer.key().to_owned();

        creator.rollback();

        {
            let state = runtime.state.lock();
            let pending = state
                .pending
                .get(&key)
                .expect("observer retains the pending");
            assert_eq!(pending.live_reservations, 1);
            assert!(!pending.committed);
        }
        assert_eq!(
            runtime.connect(7, 23, &key).unwrap(),
            RtdValue::Number(12.5)
        );
        observer.commit();
        assert!(!disconnected.load(Ordering::Acquire));
        runtime.disconnect(7, 23);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn pending_creator_rollback_and_shared_connect_are_linearizable() {
        for iteration in 0..64 {
            let runtime = Arc::new(SubscriptionRuntime::new());
            let source = Arc::new(TestSource {
                disconnected: Arc::new(AtomicBool::new(false)),
            });
            let topic = RtdTopic::single(format!("parallel-{iteration}")).unwrap();
            let creator = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
            let observer = runtime.prepare(source, topic).unwrap();
            let key = observer.key().to_owned();
            let barrier = Arc::new(std::sync::Barrier::new(3));

            let rollback_barrier = Arc::clone(&barrier);
            let rollback = std::thread::spawn(move || {
                rollback_barrier.wait();
                creator.rollback();
            });
            let connecting_runtime = Arc::clone(&runtime);
            let connect_barrier = Arc::clone(&barrier);
            let connect = std::thread::spawn(move || {
                connect_barrier.wait();
                let result = connecting_runtime.connect(9, 24, &key);
                observer.commit();
                result
            });

            barrier.wait();
            rollback.join().unwrap();
            assert_eq!(connect.join().unwrap().unwrap(), RtdValue::Number(12.5));
            runtime.disconnect(9, 24);
        }
    }

    #[test]
    fn failed_connect_applies_rollbacks_recorded_while_subscribe_is_in_flight() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let source = Arc::new(BlockingFailSource {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let topic = RtdTopic::single("in-flight-rollbacks").unwrap();
        let creator = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let observer = runtime.prepare(source, topic).unwrap();
        let key = observer.key().to_owned();
        let connecting_runtime = Arc::clone(&runtime);
        let connecting_key = key.clone();
        let connecting =
            std::thread::spawn(move || connecting_runtime.connect(11, 25, &connecting_key));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        creator.rollback();
        observer.rollback();
        {
            let state = runtime.state.lock();
            let pending = state
                .pending
                .get(&key)
                .expect("connecting placeholder remains installed");
            assert_eq!(pending.live_reservations, 0);
            assert!(!pending.committed);
            assert!(pending.connecting_generation.is_some());
        }

        release_tx.send(()).unwrap();
        assert!(matches!(
            connecting.join().unwrap(),
            Err(XllError::Domain {
                code: crate::DomainErrorCode::NativeFailure
            })
        ));
        assert!(!runtime.state.lock().pending.contains_key(&key));
    }

    #[test]
    fn failed_connect_applies_commit_recorded_while_subscribe_is_in_flight() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let source = Arc::new(BlockingFailSource {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let topic = RtdTopic::single("in-flight-commit").unwrap();
        let committed = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let failed = runtime.prepare(source, topic).unwrap();
        let key = committed.key().to_owned();
        let connecting_runtime = Arc::clone(&runtime);
        let connecting_key = key.clone();
        let connecting =
            std::thread::spawn(move || connecting_runtime.connect(12, 26, &connecting_key));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        committed.commit();
        failed.rollback();
        release_tx.send(()).unwrap();
        assert!(connecting.join().unwrap().is_err());

        let state = runtime.state.lock();
        let pending = state
            .pending
            .get(&key)
            .expect("a committed observation keeps the pending retryable");
        assert_eq!(pending.live_reservations, 0);
        assert!(pending.committed);
        assert!(pending.connecting_generation.is_none());
    }

    #[test]
    fn concurrent_connect_does_not_observe_an_uncommitted_placeholder() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let source = Arc::new(BlockingFailSource {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let prepared = runtime
            .prepare(source, RtdTopic::single("single-flight-owner").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        let connecting_runtime = Arc::clone(&runtime);
        let connecting_key = key.clone();
        let connecting =
            std::thread::spawn(move || connecting_runtime.connect(13, 27, &connecting_key));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        assert!(matches!(
            runtime.connect(13, 27, &key),
            Err(XllError::Overloaded)
        ));

        release_tx.send(()).unwrap();
        assert!(matches!(
            connecting.join().unwrap(),
            Err(XllError::Domain {
                code: crate::DomainErrorCode::NativeFailure
            })
        ));
        drop(prepared);
    }

    #[test]
    fn uncommitted_connection_rolls_back_to_a_retryable_pending_subscription() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let prepared = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("transaction-rollback").unwrap(),
            )
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();

        let connection = runtime.connect_transaction(14, 28, &key).unwrap();
        assert_eq!(connection.value(), &RtdValue::Number(12.5));
        drop(connection);

        assert!(disconnected.load(Ordering::Acquire));
        {
            let state = runtime.state.lock();
            assert!(state.active.is_empty());
            let pending = state
                .pending
                .get(&key)
                .expect("committed observation remains retryable");
            assert!(pending.committed);
            assert!(pending.connecting_generation.is_none());
        }

        let retry = runtime.connect_transaction(14, 28, &key).unwrap();
        retry.commit().unwrap();
        let state = runtime.state.lock();
        assert!(state.pending.is_empty());
        assert!(
            state
                .active
                .values()
                .all(|active| active.committed && active.subscription.is_some())
        );
    }

    #[test]
    fn failed_repeated_connection_preserves_the_existing_subscription() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let prepared = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("existing-transaction").unwrap(),
            )
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime
            .connect_transaction(15, 29, &key)
            .unwrap()
            .commit()
            .unwrap();

        let repeated = runtime.connect_transaction(15, 29, &key).unwrap();
        assert_eq!(repeated.value(), &RtdValue::Number(12.5));
        drop(repeated);

        assert!(!disconnected.load(Ordering::Acquire));
        assert_eq!(runtime.state.lock().active.len(), 1);
        runtime.disconnect(15, 29);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn abandoned_pending_leases_release_only_the_last_uncommitted_entry() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let source = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let topic = RtdTopic::single("abandoned-leases").unwrap();
        let first = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let key = first.key().to_owned();
        let second = runtime.prepare(source, topic).unwrap();

        drop(first);
        assert_eq!(runtime.state.lock().pending[&key].live_reservations, 1);
        drop(second);
        assert!(!runtime.state.lock().pending.contains_key(&key));
    }

    #[test]
    fn one_committed_observation_retains_pending_after_other_leases_fail() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let source = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let topic = RtdTopic::single("committed-pending").unwrap();
        let committed = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let key = committed.key().to_owned();
        let failed = runtime.prepare(source, topic).unwrap();

        committed.commit();
        failed.rollback();

        let state = runtime.state.lock();
        let pending = state
            .pending
            .get(&key)
            .expect("committed pending is durable");
        assert_eq!(pending.live_reservations, 0);
        assert!(pending.committed);
    }

    #[test]
    fn last_pending_rollback_drops_user_source_after_unlock_for_reentry() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (source, dropped) = reentrant_drop_source(&runtime);
        let prepared = runtime
            .prepare(source, RtdTopic::single("drop-on-rollback").unwrap())
            .unwrap();

        prepared.rollback();

        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(runtime.state.lock().pending.is_empty());
    }

    #[test]
    fn termination_drops_pending_user_source_after_unlock_for_reentry() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (source, dropped) = reentrant_drop_source(&runtime);
        let prepared = runtime
            .prepare(source, RtdTopic::single("drop-on-terminate").unwrap())
            .unwrap();
        runtime.claim_server(prepared.key(), 61).unwrap();

        drop(runtime.terminate_server(61).unwrap());

        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!runtime.state.lock().pending.contains_key(prepared.key()));
    }

    #[test]
    fn close_drops_pending_user_source_after_unlock_for_reentry() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (source, dropped) = reentrant_drop_source(&runtime);
        let prepared = runtime
            .prepare(source, RtdTopic::single("drop-on-close").unwrap())
            .unwrap();

        runtime.close().unwrap();

        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(runtime.state.lock().closed);
        assert!(!runtime.state.lock().pending.contains_key(prepared.key()));
    }

    #[test]
    fn last_pending_rollback_contains_a_panicking_source_drop() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let prepared = runtime
            .prepare(
                panicking_drop_source(&dropped),
                RtdTopic::single("panic-drop-on-rollback").unwrap(),
            )
            .unwrap();

        prepared.rollback();

        assert!(dropped.load(Ordering::Acquire));
        assert!(runtime.state.lock().pending.is_empty());
        assert!(matches!(runtime.cleanup_result(), Err(XllError::Panic)));
    }

    #[test]
    fn termination_contains_a_panicking_pending_source_drop() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let prepared = runtime
            .prepare(
                panicking_drop_source(&dropped),
                RtdTopic::single("panic-drop-on-terminate").unwrap(),
            )
            .unwrap();
        runtime.claim_server(prepared.key(), 62).unwrap();

        assert!(matches!(runtime.terminate_server(62), Err(XllError::Panic)));

        assert!(dropped.load(Ordering::Acquire));
        let state = runtime.state.lock();
        assert!(!state.terminating_servers.contains(&62));
        assert!(state.terminated_servers.contains(&62));
        assert!(!state.pending.contains_key(prepared.key()));
    }

    #[test]
    fn close_contains_each_panicking_pending_source_drop() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let first_dropped = Arc::new(AtomicBool::new(false));
        let second_dropped = Arc::new(AtomicBool::new(false));
        let first = runtime
            .prepare(
                panicking_drop_source(&first_dropped),
                RtdTopic::single("first-panic-drop-on-close").unwrap(),
            )
            .unwrap();
        let second = runtime
            .prepare(
                panicking_drop_source(&second_dropped),
                RtdTopic::single("second-panic-drop-on-close").unwrap(),
            )
            .unwrap();
        first.commit();
        second.commit();

        assert!(matches!(runtime.close(), Err(XllError::Panic)));

        assert!(first_dropped.load(Ordering::Acquire));
        assert!(second_dropped.load(Ordering::Acquire));
        let state = runtime.state.lock();
        assert!(state.closed);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn connect_contains_the_final_source_drop_panic() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let prepared = runtime
            .prepare(
                panicking_drop_source(&dropped),
                RtdTopic::single("panic-drop-after-connect").unwrap(),
            )
            .unwrap();

        assert_eq!(
            runtime.connect(63, 27, prepared.key()).unwrap(),
            RtdValue::Number(17.0)
        );

        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(runtime.state.lock().active.len(), 1);
        runtime.disconnect(63, 27);
        assert!(matches!(runtime.close(), Err(XllError::Panic)));
    }

    #[test]
    fn failed_observation_preserves_a_new_subscription_after_connect_owns_it() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let created = runtime
            .prepare(source, RtdTopic::single("owned-active").unwrap())
            .unwrap();
        runtime.connect(4, 21, created.key()).unwrap();

        created.rollback();

        let state = runtime.state.lock();
        assert!(state.pending.is_empty());
        assert_eq!(state.active.len(), 1);
        assert_eq!(state.updates.len(), 1);
        assert_eq!(state.topic_ids.len(), 1);
        drop(state);
        assert!(!disconnected.load(Ordering::Acquire));

        runtime.disconnect(4, 21);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn stale_sink_cannot_publish_after_disconnect() {
        struct CapturingSource {
            sink: Arc<Mutex<Option<RtdSink<f64>>>>,
            disconnected: Arc<AtomicBool>,
        }

        impl RtdSource for CapturingSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                *self.sink.lock() = Some(sink);
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let sink = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let key = runtime
            .prepare(
                Arc::new(CapturingSource {
                    sink: Arc::clone(&sink),
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("job-1").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 9, key.key()).unwrap();
        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();
        runtime.disconnect(1, 9);
        assert!(disconnected.load(Ordering::Acquire));
        assert!(matches!(sink.publish(2.0), Err(XllError::Closing)));
    }

    #[test]
    fn invalid_scalar_is_rejected_before_subscription_state_changes() {
        struct InvalidSource;

        impl RtdSource for InvalidSource {
            type Value = String;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                sink.publish("x".repeat(crate::utf16::EXCEL_STRING_LIMIT + 1))?;
                Ok(Box::new(TestSubscription(Arc::new(AtomicBool::new(false)))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let key = runtime
            .prepare(
                Arc::new(InvalidSource),
                RtdTopic::single("invalid").unwrap(),
            )
            .unwrap();

        assert!(runtime.connect(1, 5, key.key()).is_err());
        let state = runtime.state.lock();
        assert!(state.active.is_empty());
        assert!(state.updates.is_empty());
        assert!(state.pending.contains_key(key.key()));
    }

    #[test]
    fn committing_a_snapshot_preserves_a_newer_update() {
        struct CapturingSource {
            sink: Arc<Mutex<Option<RtdSink<f64>>>>,
        }

        impl RtdSource for CapturingSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                *self.sink.lock() = Some(sink);
                Ok(Box::new(TestSubscription(Arc::new(AtomicBool::new(false)))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let sink = Arc::new(Mutex::new(None));
        let key = runtime
            .prepare(
                Arc::new(CapturingSource {
                    sink: Arc::clone(&sink),
                }),
                RtdTopic::single("transactional").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 11, key.key()).unwrap();
        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();
        let first = runtime.snapshot_updates(1);
        sink.publish(2.0).unwrap();

        runtime.commit_updates(&first);

        let remaining = runtime.snapshot_updates(1);
        assert_eq!(remaining.updates.len(), 1);
        assert_eq!(remaining.updates[0].value, RtdValue::Number(2.0));
    }

    #[test]
    fn update_notify_failure_is_retried_and_reported() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let succeed = Arc::new(AtomicBool::new(false));

        runtime.set_notification(
            1,
            Some({
                let attempts = Arc::clone(&attempts);
                let succeed = Arc::clone(&succeed);
                Arc::new(move || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    if succeed.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(XllError::ExcelApi {
                            function: "IRTDUpdateEvent::UpdateNotify",
                            code: -2147467259, // E_FAIL
                        })
                    }
                })
            }),
        );

        let key = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("notify-fail").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 15, key.key()).unwrap();

        // Initial publish attempts bounded retries (3 times) and fails
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        // Verify pending updates remain in the queue
        assert!(runtime.has_pending_updates(1));

        // Enable success and trigger retry via heartbeat / retry_updates
        succeed.store(true, Ordering::SeqCst);
        runtime.retry_updates(1);

        // Succeeded on the 4th attempt
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn replaced_notification_can_reenter_notification_registration_from_drop() {
        struct ReentrantDrop {
            runtime: Weak<SubscriptionRuntime>,
            server_generation: u64,
            dropped: mpsc::SyncSender<()>,
        }

        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                if let Some(runtime) = self.runtime.upgrade() {
                    runtime.set_notification(self.server_generation, None);
                }
                self.dropped.send(()).unwrap();
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let reentrant = ReentrantDrop {
            runtime: Arc::downgrade(&runtime),
            server_generation: 41,
            dropped: dropped_tx,
        };
        runtime.set_notification(
            41,
            Some(Arc::new(move || {
                let _keep_drop_live = &reentrant;
                Ok(())
            })),
        );

        runtime.set_notification(41, Some(Arc::new(|| Ok(()))));

        dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!runtime.notifications.read().contains_key(&41));
    }

    #[test]
    fn close_drops_notification_callbacks_after_all_runtime_locks() {
        struct ReentrantDrop {
            runtime: Weak<SubscriptionRuntime>,
            dropped: mpsc::SyncSender<()>,
        }

        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                if let Some(runtime) = self.runtime.upgrade() {
                    runtime.set_notification(42, None);
                    assert!(runtime.state.lock().closed);
                }
                self.dropped.send(()).unwrap();
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let reentrant = ReentrantDrop {
            runtime: Arc::downgrade(&runtime),
            dropped: dropped_tx,
        };
        runtime.set_notification(
            42,
            Some(Arc::new(move || {
                let _keep_drop_live = &reentrant;
                Ok(())
            })),
        );

        runtime.close().unwrap();

        dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn termination_drops_notification_callback_after_notification_lock() {
        struct ReentrantDrop {
            runtime: Weak<SubscriptionRuntime>,
            dropped: mpsc::SyncSender<()>,
        }

        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                if let Some(runtime) = self.runtime.upgrade() {
                    runtime.set_notification(43, None);
                }
                self.dropped.send(()).unwrap();
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let reentrant = ReentrantDrop {
            runtime: Arc::downgrade(&runtime),
            dropped: dropped_tx,
        };
        runtime.set_notification(
            43,
            Some(Arc::new(move || {
                let _keep_drop_live = &reentrant;
                Ok(())
            })),
        );

        drop(runtime.terminate_server(43).unwrap());

        dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(runtime.state.lock().terminated_servers.contains(&43));
    }

    #[test]
    fn failed_refresh_can_renotify_without_consuming_the_batch() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime.set_notification(
            1,
            Some({
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        );
        let key = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("retry-refresh").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 12, key.key()).unwrap();
        let batch = runtime.snapshot_updates(1);

        runtime.retry_updates(1);

        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        assert_eq!(
            runtime.snapshot_updates(1).updates.len(),
            batch.updates.len()
        );
    }

    #[test]
    fn repeated_prepare_reuses_stable_subscription_key() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(TestSource {
            disconnected: Arc::clone(&disconnected),
        });
        let topic = RtdTopic::single("EURUSD").unwrap();

        let key1 = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();
        let key2 = runtime.prepare(Arc::clone(&source), topic.clone()).unwrap();

        assert_eq!(key1.key(), key2.key());
    }

    #[test]
    fn failed_subscribe_can_be_retried_with_the_same_key() {
        struct RetrySource {
            attempts: Arc<AtomicUsize>,
            disconnected: Arc<AtomicBool>,
        }

        impl RtdSource for RetrySource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(XllError::Domain {
                        code: crate::DomainErrorCode::NativeFailure,
                    });
                }
                sink.publish(42.0)?;
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let key = runtime
            .prepare(
                Arc::new(RetrySource {
                    attempts: Arc::clone(&attempts),
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("retry").unwrap(),
            )
            .unwrap();

        assert!(matches!(
            runtime.connect(1, 7, key.key()),
            Err(XllError::Domain {
                code: crate::DomainErrorCode::NativeFailure
            })
        ));
        assert_eq!(
            runtime.connect(1, 7, key.key()).unwrap(),
            RtdValue::Number(42.0)
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn panicking_subscribe_can_be_retried_with_the_same_key() {
        struct PanicOnceSource {
            attempts: Arc<AtomicUsize>,
        }

        impl RtdSource for PanicOnceSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected subscribe panic");
                }
                sink.publish(7.5)?;
                Ok(Box::new(TestSubscription(Arc::new(AtomicBool::new(false)))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let key = runtime
            .prepare(
                Arc::new(PanicOnceSource {
                    attempts: Arc::clone(&attempts),
                }),
                RtdTopic::single("panic-retry").unwrap(),
            )
            .unwrap();

        assert!(matches!(
            runtime.connect(1, 8, key.key()),
            Err(XllError::Panic)
        ));
        assert_eq!(
            runtime.connect(1, 8, key.key()).unwrap(),
            RtdValue::Number(7.5)
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn topic_ids_cannot_be_reused_for_a_different_subscription() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let first = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("first").unwrap(),
            )
            .unwrap();
        let second = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("second").unwrap(),
            )
            .unwrap();

        runtime.connect(1, 7, first.key()).unwrap();
        assert!(matches!(
            runtime.connect(1, 7, second.key()),
            Err(XllError::InvalidHandle)
        ));
        assert_eq!(runtime.state.lock().active.len(), 1);
    }

    #[test]
    fn terminating_an_old_server_generation_preserves_new_topics() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let old_key = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("old").unwrap(),
            )
            .unwrap();
        let new_key = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("new").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 4, old_key.key()).unwrap();
        runtime.connect(2, 4, new_key.key()).unwrap();

        runtime.terminate_server(1).unwrap();

        let state = runtime.state.lock();
        assert!(!state.active.contains_key(&TopicOwner {
            server_generation: 1,
            topic_id: 4,
        }));
        assert!(state.active.contains_key(&TopicOwner {
            server_generation: 2,
            topic_id: 4,
        }));
    }

    #[test]
    fn claim_rejects_a_generation_after_termination_has_scanned_pending() {
        struct BlockingDisconnectSource {
            entered: mpsc::SyncSender<()>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        }

        struct BlockingDisconnectSubscription {
            entered: mpsc::SyncSender<()>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        }

        // SAFETY: disconnect_and_wait ensures no background work accesses module code.
        unsafe impl RtdSubscription for BlockingDisconnectSubscription {
            fn request_cancel(&self) {}

            fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(())
            }
        }

        impl RtdSource for BlockingDisconnectSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                _sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(BlockingDisconnectSubscription {
                    entered: self.entered.clone(),
                    release: Arc::clone(&self.release),
                }))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let active = runtime
            .prepare(
                Arc::new(BlockingDisconnectSource {
                    entered: entered_tx,
                    release: Arc::new(Mutex::new(release_rx)),
                }),
                RtdTopic::single("termination-barrier").unwrap(),
            )
            .unwrap();
        runtime.connect(31, 1, active.key()).unwrap();
        let pending = runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("unclaimed-during-termination").unwrap(),
            )
            .unwrap();
        let pending_key = pending.key().to_owned();

        let terminating_runtime = Arc::clone(&runtime);
        let terminating =
            std::thread::spawn(move || drop(terminating_runtime.terminate_server(31).unwrap()));
        // disconnect_and_wait runs after terminate_server's pending scan.
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        assert!(matches!(
            runtime.claim_server(&pending_key, 31),
            Err(XllError::Closing)
        ));
        pending.commit();
        release_tx.send(()).unwrap();
        terminating.join().unwrap();

        assert!(runtime.claim_server(&pending_key, 32).is_ok());
        assert_eq!(
            runtime.state.lock().pending[&pending_key].server_generation,
            Some(32)
        );
    }

    #[test]
    fn expired_source_identities_are_reclaimed_and_close_clears_them() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let first_source = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let first_key = runtime
            .prepare(first_source.clone(), RtdTopic::single("first").unwrap())
            .unwrap();
        first_key.rollback();
        drop(first_source);

        runtime
            .prepare(
                Arc::new(TestSource {
                    disconnected: Arc::new(AtomicBool::new(false)),
                }),
                RtdTopic::single("second").unwrap(),
            )
            .unwrap();
        assert_eq!(runtime.state.lock().source_ids.len(), 1);

        runtime.close().unwrap();
        assert!(runtime.state.lock().source_ids.is_empty());
    }

    #[test]
    fn close_reports_a_panicking_cancellation_hook() {
        struct Source;
        struct Subscription;

        // SAFETY: disconnect_and_wait ensures no background work accesses module code.
        unsafe impl RtdSubscription for Subscription {
            fn request_cancel(&self) {
                panic!("injected cancellation panic");
            }

            fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
                Ok(())
            }
        }

        impl RtdSource for Source {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                _sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(Subscription))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let prepared = runtime
            .prepare(Arc::new(Source), RtdTopic::single("cancel-panic").unwrap())
            .unwrap();
        runtime.connect(1, 1, prepared.key()).unwrap();

        assert!(matches!(
            runtime.close(),
            Err(XllError::RtdSubscriptionShutdown { source, .. })
                if matches!(*source, XllError::Panic)
        ));
    }

    #[test]
    fn close_reports_a_failed_disconnect_wait() {
        struct Source;
        struct Subscription;

        // SAFETY: disconnect_and_wait ensures no background work accesses module code.
        unsafe impl RtdSubscription for Subscription {
            fn request_cancel(&self) {}

            fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
                Err(XllError::Internal {
                    diagnostic_id: 0x4449_5343_4641_494c,
                })
            }
        }

        impl RtdSource for Source {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                _sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(Subscription))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let prepared = runtime
            .prepare(
                Arc::new(Source),
                RtdTopic::single("disconnect-failure").unwrap(),
            )
            .unwrap();
        runtime.connect(1, 1, prepared.key()).unwrap();

        assert!(matches!(
            runtime.close(),
            Err(XllError::RtdSubscriptionShutdown { source, .. })
                if matches!(
                    *source,
                    XllError::Internal {
                        diagnostic_id: 0x4449_5343_4641_494c
                    }
                )
        ));
    }

    #[test]
    fn close_cancels_every_subscription_before_the_first_wait() {
        struct ContractSource {
            id: usize,
            canceled: Arc<Vec<AtomicBool>>,
            waited: Arc<AtomicUsize>,
        }

        struct ContractSubscription {
            id: usize,
            canceled: Arc<Vec<AtomicBool>>,
            waited: Arc<AtomicUsize>,
        }

        // SAFETY: disconnect_and_wait ensures no background work accesses module code.
        unsafe impl RtdSubscription for ContractSubscription {
            fn request_cancel(&self) {
                self.canceled[self.id].store(true, Ordering::Release);
            }

            fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
                assert!(
                    self.canceled
                        .iter()
                        .all(|canceled| canceled.load(Ordering::Acquire)),
                    "all cancellation requests must precede the first blocking wait"
                );
                self.waited.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
        }

        impl RtdSource for ContractSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                _sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(ContractSubscription {
                    id: self.id,
                    canceled: Arc::clone(&self.canceled),
                    waited: Arc::clone(&self.waited),
                }))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let canceled = Arc::new((0..3).map(|_| AtomicBool::new(false)).collect::<Vec<_>>());
        let waited = Arc::new(AtomicUsize::new(0));
        for id in 0..3 {
            let key = runtime
                .prepare(
                    Arc::new(ContractSource {
                        id,
                        canceled: Arc::clone(&canceled),
                        waited: Arc::clone(&waited),
                    }),
                    RtdTopic::single(format!("contract-{id}")).unwrap(),
                )
                .unwrap();
            runtime.connect(8, id as i32, key.key()).unwrap();
        }

        runtime.close().unwrap();

        assert!(canceled.iter().all(|flag| flag.load(Ordering::Acquire)));
        assert_eq!(waited.load(Ordering::Acquire), 3);
    }

    #[test]
    fn close_waits_for_subscribe_initialization_and_its_cleanup() {
        struct BlockingSource {
            entered: mpsc::SyncSender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            disconnected: Arc<AtomicBool>,
        }

        impl RtdSource for BlockingSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                _sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let disconnected = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let key = runtime
            .prepare(
                Arc::new(BlockingSource {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("blocking-subscribe").unwrap(),
            )
            .unwrap();

        let connecting_runtime = Arc::clone(&runtime);
        let connecting = std::thread::spawn(move || connecting_runtime.connect(1, 17, key.key()));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closing_runtime = Arc::clone(&runtime);
        let closing = std::thread::spawn(move || {
            closing_runtime.close().unwrap();
            closed_tx.send(()).unwrap();
        });
        wait_until(|| runtime.state.lock().closed);

        assert_eq!(closed_rx.try_recv(), Err(TryRecvError::Empty));
        assert!(!disconnected.load(Ordering::Acquire));

        release_tx.send(()).unwrap();
        assert!(matches!(connecting.join().unwrap(), Err(XllError::Closing)));
        closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        closing.join().unwrap();

        assert!(disconnected.load(Ordering::Acquire));
        assert_eq!(runtime.state.lock().in_flight, 0);
    }

    #[test]
    fn close_waits_for_a_cloned_notification_callback() {
        struct CapturingSource {
            sink: Arc<Mutex<Option<RtdSink<f64>>>>,
            disconnected: Arc<AtomicBool>,
        }

        impl RtdSource for CapturingSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                *self.sink.lock() = Some(sink);
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let sink = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let key = runtime
            .prepare(
                Arc::new(CapturingSource {
                    sink: Arc::clone(&sink),
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("blocking-notify").unwrap(),
            )
            .unwrap();
        runtime.connect(2, 18, key.key()).unwrap();
        let sink = sink.lock().clone().unwrap();

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        runtime.set_notification(
            2,
            Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(())
            })),
        );

        let publishing_sink = sink.clone();
        let publishing = std::thread::spawn(move || publishing_sink.publish(42.0));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closing_runtime = Arc::clone(&runtime);
        let closing = std::thread::spawn(move || {
            closing_runtime.close().unwrap();
            closed_tx.send(()).unwrap();
        });
        wait_until(|| runtime.state.lock().closed);

        assert_eq!(closed_rx.try_recv(), Err(TryRecvError::Empty));
        release_tx.send(()).unwrap();
        publishing.join().unwrap().unwrap();
        closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        closing.join().unwrap();

        assert!(disconnected.load(Ordering::Acquire));
        assert!(matches!(sink.publish(43.0), Err(XllError::Closing)));
    }

    #[test]
    fn server_termination_waits_for_generation_notifications() {
        struct CapturingSource {
            sink: Arc<Mutex<Option<RtdSink<f64>>>>,
            disconnected: Arc<AtomicBool>,
        }

        impl RtdSource for CapturingSource {
            type Value = f64;

            fn subscribe(
                &self,
                _topic: &RtdTopic,
                sink: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                *self.sink.lock() = Some(sink);
                Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
            }
        }

        let runtime = Arc::new(SubscriptionRuntime::new());
        let sink = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let key = runtime
            .prepare(
                Arc::new(CapturingSource {
                    sink: Arc::clone(&sink),
                    disconnected: Arc::clone(&disconnected),
                }),
                RtdTopic::single("terminate-notify").unwrap(),
            )
            .unwrap();
        runtime.connect(3, 19, key.key()).unwrap();
        let sink = sink.lock().clone().unwrap();

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        runtime.set_notification(
            3,
            Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(())
            })),
        );

        let publishing_sink = sink.clone();
        let publishing = std::thread::spawn(move || publishing_sink.publish(7.0));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (terminated_tx, terminated_rx) = mpsc::sync_channel(1);
        let terminating_runtime = Arc::clone(&runtime);
        let terminating = std::thread::spawn(move || {
            drop(terminating_runtime.terminate_server(3).unwrap());
            terminated_tx.send(()).unwrap();
        });
        wait_until(|| runtime.state.lock().terminating_servers.contains(&3));

        assert_eq!(terminated_rx.try_recv(), Err(TryRecvError::Empty));
        assert!(matches!(
            runtime.terminate_server(3),
            Err(XllError::Closing)
        ));
        release_tx.send(()).unwrap();
        publishing.join().unwrap().unwrap();
        terminated_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        terminating.join().unwrap();

        assert!(disconnected.load(Ordering::Acquire));
        assert!(runtime.state.lock().terminated_servers.contains(&3));
        assert!(matches!(sink.publish(8.0), Err(XllError::Closing)));
    }
}
