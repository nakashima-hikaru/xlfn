#![cfg_attr(
    not(target_os = "windows"),
    allow(dead_code, reason = "Internal helpers for Windows COM integration")
)]

use crate::{ExcelErrorValue, XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

const DEFAULT_MAX_RTD_TOPIC_PARTS: usize = 253;
const DEFAULT_MAX_RTD_TOPIC_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_RTD_PENDING: usize = 4096;
const DEFAULT_MAX_RTD_ACTIVE: usize = 4096;
const DEFAULT_MAX_RTD_QUEUED_UPDATES: usize = 4096;
const DEFAULT_MAX_RTD_SOURCE_IDS: usize = 4096;
const DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES: usize = 64 * 1024 * 1024;

/// Resource limits for one add-in's RTD subscription runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtdLimits {
    pub max_topic_parts: usize,
    pub max_topic_bytes: usize,
    pub max_pending: usize,
    pub max_active: usize,
    pub max_queued_updates: usize,
    pub max_source_ids: usize,
    pub max_total_topic_bytes: usize,
}

impl RtdLimits {
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            max_topic_parts: DEFAULT_MAX_RTD_TOPIC_PARTS,
            max_topic_bytes: DEFAULT_MAX_RTD_TOPIC_BYTES,
            max_pending: DEFAULT_MAX_RTD_PENDING,
            max_active: DEFAULT_MAX_RTD_ACTIVE,
            max_queued_updates: DEFAULT_MAX_RTD_QUEUED_UPDATES,
            max_source_ids: DEFAULT_MAX_RTD_SOURCE_IDS,
            max_total_topic_bytes: DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES,
        }
    }
}

impl Default for RtdLimits {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ServerGeneration(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TopicId(pub(crate) i32);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionKey(pub(crate) Arc<str>);

impl SubscriptionKey {
    pub(crate) fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }
}

impl std::ops::Deref for SubscriptionKey {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<&str> for SubscriptionKey {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for SubscriptionKey {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnectionGeneration(pub(crate) u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RtdTopic {
    parts: Arc<[String]>,
}

impl RtdTopic {
    pub fn new(parts: impl IntoIterator<Item = impl Into<String>>) -> XllResult<Self> {
        let limits = RtdLimits::standard();
        let mut normalized = Vec::new();
        let mut total_bytes = 0_usize;
        for part in parts {
            if normalized.len() >= limits.max_topic_parts {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_parts,
                        actual: normalized.len().saturating_add(1),
                    },
                ));
            }
            let part = part.into();
            if part.is_empty() {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::Malformed("RTD topics require non-empty parts"),
                ));
            }
            let utf16_len = part.encode_utf16().count();
            if utf16_len > 32_767 {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: 32_767,
                        actual: utf16_len,
                    },
                ));
            }
            total_bytes = total_bytes.checked_add(part.len()).ok_or_else(|| {
                XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_bytes,
                        actual: usize::MAX,
                    },
                )
            })?;
            if total_bytes > limits.max_topic_bytes {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_bytes,
                        actual: total_bytes,
                    },
                ));
            }
            normalized.push(part);
        }
        if normalized.is_empty() {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::Malformed("RTD topics require non-empty parts"),
            ));
        }
        Ok(Self {
            parts: Arc::from(normalized),
        })
    }

    pub fn single(part: impl Into<String>) -> XllResult<Self> {
        Self::new([part.into()])
    }

    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    fn validate_with_limits(&self, limits: &RtdLimits) -> XllResult<()> {
        validate_topic_parts(&self.parts, limits)
    }

    fn byte_len(&self) -> usize {
        self.parts.iter().map(String::len).sum()
    }
}

fn validate_topic_parts(parts: &[String], limits: &RtdLimits) -> XllResult<()> {
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::Malformed("RTD topics require non-empty parts"),
        ));
    }
    if parts.len() > limits.max_topic_parts {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::TooLarge {
                limit: limits.max_topic_parts,
                actual: parts.len(),
            },
        ));
    }

    let mut total_bytes = 0_usize;
    for part in parts {
        let utf16_len = part.encode_utf16().count();
        if utf16_len > 32_767 {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::TooLarge {
                    limit: 32_767,
                    actual: utf16_len,
                },
            ));
        }
        total_bytes = total_bytes.checked_add(part.len()).ok_or_else(|| {
            XllError::input(
                "RTD topic",
                crate::InputError::TooLarge {
                    limit: limits.max_topic_bytes,
                    actual: usize::MAX,
                },
            )
        })?;
    }
    if total_bytes > limits.max_topic_bytes {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::TooLarge {
                limit: limits.max_topic_bytes,
                actual: total_bytes,
            },
        ));
    }
    Ok(())
}

/// # Safety
/// Implementors must ensure that cancellation or disconnection can be safely initiated from any thread.
pub unsafe trait RtdSubscription: Send + 'static {
    fn request_cancel(&self);
    fn disconnect_and_wait(self: Box<Self>) -> XllResult<()>;
}

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

    fn into_excel(self, _: &mut crate::ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
    }
}

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
    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>>;
}

impl<S: RtdSource + ?Sized> RtdSource for Arc<S> {
    type Value = S::Value;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>> {
        (**self).subscribe(topic, sink)
    }
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

pub(crate) struct Quota {
    used: AtomicUsize,
    limit: usize,
}

impl Quota {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    pub(crate) fn try_acquire(self: &Arc<Self>) -> XllResult<QuotaPermit> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .map_err(|_| XllError::Overloaded)?;

        Ok(QuotaPermit {
            quota: Arc::clone(self),
        })
    }
}

pub(crate) struct QuotaPermit {
    quota: Arc<Quota>,
}

impl Drop for QuotaPermit {
    fn drop(&mut self) {
        let previous = self.quota.used.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0, "quota permit drop underflow");
    }
}

pub(crate) struct OperationGate {
    state: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

const CLOSING_BIT: usize = usize::MAX / 2 + 1;

impl OperationGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        })
    }

    pub(crate) fn enter(self: &Arc<Self>) -> XllResult<OperationGuard> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |val| {
                if (val & CLOSING_BIT) != 0 {
                    None
                } else {
                    Some(val + 1)
                }
            })
            .map_err(|_| XllError::Closing)?;

        Ok(OperationGuard {
            gate: Arc::clone(self),
        })
    }

    pub(crate) fn close_and_wait_begin(&self) -> TerminationWaitGuard<'_> {
        self.state.fetch_or(CLOSING_BIT, Ordering::AcqRel);
        TerminationWaitGuard { gate: self }
    }

    fn leave(&self) {
        let prev = self.state.fetch_sub(1, Ordering::AcqRel);
        let active_count = (prev & !CLOSING_BIT) - 1;
        if active_count == 0 && (prev & CLOSING_BIT) != 0 {
            let _guard = self.wait_lock.lock();
            self.idle.notify_all();
        }
    }
}

pub(crate) struct OperationGuard {
    gate: Arc<OperationGate>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.gate.leave();
    }
}

pub(crate) struct TerminationWaitGuard<'a> {
    gate: &'a OperationGate,
}

impl TerminationWaitGuard<'_> {
    pub(crate) fn wait(self) {
        let mut guard = self.gate.wait_lock.lock();
        while (self.gate.state.load(Ordering::Acquire) & !CLOSING_BIT) > 0 {
            self.gate.idle.wait(&mut guard);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BindingStage {
    Connecting,
    Active,
}

struct ActiveKeyBinding {
    connection_generation: ConnectionGeneration,
    stage: BindingStage,
}

struct PendingSubscription {
    live_reservations: usize,
    committed: bool,
    source: Arc<dyn ErasedRtdSource>,
    topic: RtdTopic,
    server_generation: Option<ServerGeneration>,
    connecting_generation: Option<ConnectionGeneration>,
}

struct SubscriptionCatalog {
    pending: HashMap<SubscriptionKey, PendingSubscription>,
    pending_topic_bytes: usize,
    source_ids: HashMap<usize, u64>,
    active_keys: HashMap<SubscriptionKey, ActiveKeyBinding>,
}

#[derive(Clone, Debug)]
pub(crate) struct ErasedSink {
    server: Weak<ServerRuntime>,
    topic_id: TopicId,
    connection_generation: ConnectionGeneration,
}

impl ErasedSink {
    fn publish(&self, value: RtdValue) -> XllResult<()> {
        let server = self.server.upgrade().ok_or(XllError::Closing)?;
        server.publish(self.topic_id, self.connection_generation, value)
    }
}

struct ActiveSubscription {
    key: SubscriptionKey,
    generation: ConnectionGeneration,
    subscription: Option<Box<dyn RtdSubscription>>,
    committed: bool,
    latest: RtdValue,
    _permit: QuotaPermit,
}

struct QueuedUpdate {
    connection_generation: ConnectionGeneration,
    sequence: u64,
    value: RtdValue,
    _permit: QuotaPermit,
}

pub(crate) struct RtdUpdate {
    sequence: u64,
    pub(crate) topic_id: i32,
    pub(crate) value: RtdValue,
}

#[cfg(test)]
impl RtdUpdate {
    pub(crate) fn for_test(topic_id: i32, value: RtdValue) -> Self {
        Self {
            sequence: 0,
            topic_id,
            value,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefreshOutcome {
    Delivered,
    Failed,
}

type NotificationCallback = Arc<dyn Fn() -> XllResult<()> + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalState {
    Dormant,
    Calling { ticket: u64, attempt: u8 },
    Signaled { ticket: u64 },
    Suppressed { ticket: u64 },
}

#[derive(Debug)]
enum DeliveryPhase {
    BetweenRefreshes { signal: SignalState },
    Refreshing { refresh_id: u64 },
}

impl Default for DeliveryPhase {
    fn default() -> Self {
        Self::BetweenRefreshes {
            signal: SignalState::Dormant,
        }
    }
}

#[derive(Default)]
struct ServerDeliveryState {
    callback: Option<NotificationCallback>,
    updates: BTreeMap<TopicId, QueuedUpdate>,
    next_update_sequence: u64,
    next_notification_ticket: u64,
    next_refresh_id: u64,
    phase: DeliveryPhase,
}

#[derive(Clone)]
struct NotificationAttempt {
    ticket: u64,
    callback: NotificationCallback,
}

struct PreparedNotification {
    ticket: u64,
    callback: NotificationCallback,
}

enum NotificationCompletion {
    Finished,
    Retry(NotificationAttempt),
    Failed(XllError),
}

impl ServerDeliveryState {
    fn allocate_update_sequence(&mut self) -> XllResult<u64> {
        let seq = self.next_update_sequence;
        self.next_update_sequence = seq.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5345_514f_5646_4c57,
        })?;
        Ok(seq)
    }

    fn allocate_refresh_id(&mut self) -> XllResult<u64> {
        let id = self.next_refresh_id;
        self.next_refresh_id = id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5245_464f_5646_4c57,
        })?;
        Ok(id)
    }

    fn reset_suppressed_signal(&mut self) {
        if let DeliveryPhase::BetweenRefreshes {
            signal: signal @ SignalState::Suppressed { .. },
        } = &mut self.phase
        {
            *signal = SignalState::Dormant;
        }
    }

    fn attach_callback(&mut self, callback: NotificationCallback) -> Option<NotificationCallback> {
        let retired = self.callback.replace(callback);
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    fn detach_callback(&mut self) -> Option<NotificationCallback> {
        let retired = self.callback.take();
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    fn prepare_notification(
        &self,
        has_pending_updates: bool,
    ) -> XllResult<Option<PreparedNotification>> {
        if !has_pending_updates {
            return Ok(None);
        }
        let DeliveryPhase::BetweenRefreshes { signal } = &self.phase else {
            return Ok(None);
        };
        if !matches!(signal, SignalState::Dormant) {
            return Ok(None);
        }
        let Some(callback) = self.callback.as_ref().cloned() else {
            return Ok(None);
        };
        let ticket = self.next_notification_ticket;
        let _next = ticket.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5449_434b_4f56_464c,
        })?;
        Ok(Some(PreparedNotification { ticket, callback }))
    }

    fn commit_notification(&mut self, prepared: PreparedNotification) -> NotificationAttempt {
        self.next_notification_ticket = prepared.ticket + 1;
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Calling {
                ticket: prepared.ticket,
                attempt: 0,
            };
        }
        NotificationAttempt {
            ticket: prepared.ticket,
            callback: prepared.callback,
        }
    }

    fn signal_calling_mut(&mut self, ticket: u64) -> Option<&mut SignalState> {
        let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase else {
            return None;
        };
        if matches!(signal, SignalState::Calling { ticket: t, .. } if *t == ticket) {
            Some(signal)
        } else {
            None
        }
    }

    fn signal_for_ticket_mut(&mut self, ticket: u64) -> Option<&mut SignalState> {
        let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase else {
            return None;
        };
        match signal {
            SignalState::Calling { ticket: t, .. } | SignalState::Signaled { ticket: t }
                if *t == ticket =>
            {
                Some(signal)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerLifecycle {
    Open,
    Closing,
    Terminated,
}

struct ServerState {
    lifecycle: ServerLifecycle,
    active_by_topic: HashMap<TopicId, ActiveSubscription>,
    topic_by_key: HashMap<SubscriptionKey, TopicId>,
    delivery: ServerDeliveryState,
}

impl ServerState {
    fn ensure_open(&self) -> XllResult<()> {
        if self.lifecycle == ServerLifecycle::Open {
            Ok(())
        } else {
            Err(XllError::Closing)
        }
    }

    fn has_deliverable_updates(&self) -> bool {
        self.delivery.updates.keys().any(|topic_id| {
            self.active_by_topic
                .get(topic_id)
                .is_some_and(|active| active.committed)
        })
    }
}

#[derive(Clone)]
pub(crate) struct RtdServerHandle {
    inner: Arc<ServerRuntime>,
}

impl RtdServerHandle {
    pub(crate) fn attach_update_callback(
        &self,
        callback: NotificationCallback,
    ) -> XllResult<Option<NotificationCallback>> {
        let _operation = self.inner.enter_operation()?;
        let (retired, attempt) = {
            let mut state = self.inner.state.lock();
            state.ensure_open()?;
            let retired = state.delivery.attach_callback(callback);
            let has_updates = state.has_deliverable_updates();
            let prepared = state.delivery.prepare_notification(has_updates)?;
            let attempt = prepared.map(|p| state.delivery.commit_notification(p));
            (retired, attempt)
        };
        if let Some(attempt) = attempt {
            self.inner.drive_notification(attempt);
        }
        Ok(retired)
    }

    pub(crate) fn detach_update_callback(&self) -> Option<NotificationCallback> {
        let mut state = self.inner.state.lock();
        state.delivery.detach_callback()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        let attempt = {
            let mut state = self.inner.state.lock();
            state.ensure_open()?;
            let has_updates = state.has_deliverable_updates();
            let prepared = state.delivery.prepare_notification(has_updates)?;
            prepared.map(|p| state.delivery.commit_notification(p))
        };
        if let Some(attempt) = attempt {
            self.inner.drive_notification(attempt);
        }
        Ok(())
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch> {
        let operation = self.inner.enter_operation()?;
        let (refresh_id, updates) = {
            let mut state = self.inner.state.lock();
            state.ensure_open()?;
            if matches!(state.delivery.phase, DeliveryPhase::Refreshing { .. }) {
                return Err(XllError::Internal {
                    diagnostic_id: 0x4f56_4c50_5245_4652,
                });
            }
            let refresh_id = state.delivery.allocate_refresh_id()?;
            let ServerState {
                active_by_topic,
                delivery,
                ..
            } = &mut *state;
            let updates = delivery
                .updates
                .iter()
                .filter(|&(&topic_id, _)| {
                    active_by_topic
                        .get(&topic_id)
                        .is_some_and(|active| active.committed)
                })
                .map(|(&topic_id, queued)| RtdUpdate {
                    sequence: queued.sequence,
                    topic_id: topic_id.0,
                    value: queued.value.clone(),
                })
                .collect();
            delivery.phase = DeliveryPhase::Refreshing { refresh_id };
            (refresh_id, updates)
        };
        Ok(RtdRefreshBatch {
            server: Arc::clone(&self.inner),
            operation: Some(operation),
            refresh_id,
            updates,
            completed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        let state = self.inner.state.lock();
        state
            .delivery
            .updates
            .keys()
            .filter(|topic_id| {
                state
                    .active_by_topic
                    .get(topic_id)
                    .is_some_and(|a| a.committed)
            })
            .count()
    }

    pub(crate) fn claim(&self, key: &SubscriptionKey) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.claim_server_key(self.inner.generation, key)
    }

    pub(crate) fn connect_transaction(
        &self,
        topic_id: TopicId,
        key: &SubscriptionKey,
    ) -> XllResult<SubscriptionConnection> {
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.connect_transaction(self, topic_id, key)
    }

    pub(crate) fn disconnect(&self, topic_id: TopicId) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.disconnect(self, topic_id)
    }

    pub(crate) fn terminate(&self) -> XllResult<()> {
        self.inner.terminate()
    }
}

pub(crate) struct ServerRuntime {
    generation: ServerGeneration,
    module_ingress: Option<&'static crate::ingress::ExportIngress>,
    operation_gate: Arc<OperationGate>,
    state: Mutex<ServerState>,
    parent: Weak<SubscriptionRuntime>,
    termination_coordinator: TerminationCoordinator,
}

pub(crate) struct ServerOperation {
    _gate_guard: OperationGuard,
    _ingress_guard: Option<crate::ingress::ExportCallGuard<'static>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    parent: Weak<SubscriptionRuntime>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl Drop for ServerOperation {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.upgrade() {
            parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

impl ServerRuntime {
    fn enter_operation(&self) -> XllResult<ServerOperation> {
        let parent = self.parent.upgrade().ok_or(XllError::Closing)?;
        if (parent.runtime_gate.state.load(Ordering::Acquire) & CLOSING_BIT) != 0 {
            return Err(XllError::Closing);
        }

        if let Some(ingress) = self.module_ingress {
            let mut gate_guard = None;
            let mut gate_error = None;
            let (ingress_guard, accepted) =
                ingress.enter_with(|| match self.operation_gate.enter() {
                    Ok(guard) => {
                        gate_guard = Some(guard);
                        #[cfg(any(test, feature = "shutdown-refinement"))]
                        if let Some(parent) = self.parent.upgrade() {
                            parent.record_ghost_event(
                                crate::shutdown_refinement::GhostEvent::BeginRtdOperation,
                            );
                        }
                    }
                    Err(err) => gate_error = Some(err),
                });
            if !accepted {
                return Err(XllError::Closing);
            }
            if let Some(err) = gate_error {
                drop(ingress_guard);
                return Err(err);
            }
            Ok(ServerOperation {
                _gate_guard: gate_guard.expect("gate guard is acquired"),
                _ingress_guard: Some(ingress_guard),
                #[cfg(any(test, feature = "shutdown-refinement"))]
                parent: self.parent.clone(),
            })
        } else {
            let gate_guard = self.operation_gate.enter()?;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
            }
            Ok(ServerOperation {
                _gate_guard: gate_guard,
                _ingress_guard: None,
                #[cfg(any(test, feature = "shutdown-refinement"))]
                parent: self.parent.clone(),
            })
        }
    }

    fn publish(
        self: &Arc<Self>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        value: RtdValue,
    ) -> XllResult<()> {
        let _operation = self.enter_operation()?;
        let attempt = {
            let mut state = self.state.lock();
            state.ensure_open()?;
            let active = state
                .active_by_topic
                .get(&topic_id)
                .filter(|active| active.generation == generation)
                .ok_or(XllError::Closing)?;

            let committed = active.committed;
            let is_new_update = !state.delivery.updates.contains_key(&topic_id);

            let parent = self.parent.upgrade().ok_or(XllError::Closing)?;
            let permit = if is_new_update {
                Some(parent.queued_update_quota.try_acquire()?)
            } else {
                None
            };

            let conn_gen = active.generation;
            let sequence = state.delivery.allocate_update_sequence()?;
            let prepared = if committed {
                state.delivery.prepare_notification(true)?
            } else {
                None
            };

            match state.delivery.updates.entry(topic_id) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    existing.connection_generation = conn_gen;
                    existing.sequence = sequence;
                    existing.value = value.clone();
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(QueuedUpdate {
                        connection_generation: conn_gen,
                        sequence,
                        value: value.clone(),
                        _permit: permit.expect("new update owns quota permit"),
                    });
                }
            }

            state
                .active_by_topic
                .get_mut(&topic_id)
                .expect("active topic was validated above")
                .latest = value;

            prepared.map(|prepared| state.delivery.commit_notification(prepared))
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
        Ok(())
    }

    fn drive_notification(self: &Arc<Self>, mut attempt: NotificationAttempt) {
        loop {
            let res = catch_unwind(AssertUnwindSafe(|| (attempt.callback)()));
            let completion = match res {
                Ok(Ok(())) => self.finish_notification_attempt(attempt.ticket, Ok(())),
                Ok(Err(err)) => self.finish_notification_attempt(attempt.ticket, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: 0x5041_4e49_434e_4f54,
                    };
                    if let Some(parent) = self.parent.upgrade() {
                        parent.record_cleanup_result(Err(err.clone()));
                    }
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                NotificationCompletion::Finished => break,
                NotificationCompletion::Retry(next) => attempt = next,
                NotificationCompletion::Failed(err) => {
                    if let Some(parent) = self.parent.upgrade() {
                        parent.record_cleanup_result(Err(err));
                    }
                    break;
                }
            }
        }
    }

    fn finish_notification_attempt(
        &self,
        ticket: u64,
        outcome: XllResult<()>,
    ) -> NotificationCompletion {
        let mut state = self.state.lock();
        let callback = state.delivery.callback.clone();
        let Some(signal) = state.delivery.signal_for_ticket_mut(ticket) else {
            return NotificationCompletion::Finished;
        };

        let attempt = match signal {
            SignalState::Calling { ticket: t, attempt } if *t == ticket => *attempt,
            _ => return NotificationCompletion::Finished,
        };

        match outcome {
            Ok(()) => {
                if let Some(signal) = state.delivery.signal_calling_mut(ticket) {
                    *signal = SignalState::Signaled { ticket };
                }
                NotificationCompletion::Finished
            }
            Err(error) => {
                if attempt < 2 {
                    let next_attempt = attempt + 1;
                    if let Some(signal) = state.delivery.signal_calling_mut(ticket) {
                        *signal = SignalState::Calling {
                            ticket,
                            attempt: next_attempt,
                        };
                    }
                    if let Some(callback) = callback {
                        NotificationCompletion::Retry(NotificationAttempt { ticket, callback })
                    } else {
                        state.delivery.reset_suppressed_signal();
                        NotificationCompletion::Failed(error)
                    }
                } else {
                    if let Some(signal) = state.delivery.signal_calling_mut(ticket) {
                        *signal = SignalState::Suppressed { ticket };
                    }
                    NotificationCompletion::Failed(error)
                }
            }
        }
    }

    fn complete_refresh_inner(
        &self,
        refresh_id: u64,
        delivered_updates: &[RtdUpdate],
        outcome: RefreshOutcome,
    ) -> XllResult<Option<NotificationAttempt>> {
        let mut state = self.state.lock();
        let DeliveryPhase::Refreshing {
            refresh_id: active_id,
        } = state.delivery.phase
        else {
            return Err(XllError::Internal {
                diagnostic_id: 0x4e4f_5245_4652_4143,
            });
        };

        if active_id != refresh_id {
            return Err(XllError::Internal {
                diagnostic_id: 0x5245_4652_4944_4d49,
            });
        }

        match outcome {
            RefreshOutcome::Delivered => {
                for update in delivered_updates {
                    let topic_id = TopicId(update.topic_id);
                    if state
                        .delivery
                        .updates
                        .get(&topic_id)
                        .is_some_and(|u| u.sequence == update.sequence)
                    {
                        state.delivery.updates.remove(&topic_id);
                    }
                }
            }
            RefreshOutcome::Failed => {}
        }

        state.delivery.phase = DeliveryPhase::BetweenRefreshes {
            signal: SignalState::Dormant,
        };

        let has_updates = state.has_deliverable_updates();
        let prepared = state.delivery.prepare_notification(has_updates)?;
        let attempt = prepared.map(|p| state.delivery.commit_notification(p));
        Ok(attempt)
    }

    fn abort_refresh_no_unwind(self: &Arc<Self>, refresh_id: u64) {
        let attempt = {
            let mut state = self.state.lock();
            if let DeliveryPhase::Refreshing {
                refresh_id: active_id,
            } = state.delivery.phase
            {
                if active_id == refresh_id {
                    state.delivery.phase = DeliveryPhase::BetweenRefreshes {
                        signal: SignalState::Dormant,
                    };
                    let has_updates = state.has_deliverable_updates();
                    let prepared = state
                        .delivery
                        .prepare_notification(has_updates)
                        .ok()
                        .flatten();
                    prepared.map(|p| state.delivery.commit_notification(p))
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
    }

    fn remove_from_registry(&self) {
        if let Some(parent) = self.parent.upgrade() {
            let mut servers = parent.servers.lock();
            servers.remove(&self.generation);
        }
    }

    fn begin_termination<'a>(self: &'a Arc<Self>) -> TerminationAdmission<'a> {
        let mut term_state = self.termination_coordinator.state.lock();
        match *term_state {
            ServerTerminationPhase::Terminated => TerminationAdmission::Complete,
            ServerTerminationPhase::Terminating => {
                TerminationAdmission::Waiter(ServerTerminationWaiter {
                    coordinator: &self.termination_coordinator,
                })
            }
            ServerTerminationPhase::Open => {
                let wait = self.operation_gate.close_and_wait_begin();
                *term_state = ServerTerminationPhase::Terminating;

                let (callback, initial_subscriptions) = {
                    let mut state = self.state.lock();
                    debug_assert_eq!(state.lifecycle, ServerLifecycle::Open);
                    state.lifecycle = ServerLifecycle::Closing;

                    let callback = state.delivery.detach_callback();
                    state.delivery.updates.clear();

                    let initial_subscriptions = state
                        .active_by_topic
                        .values_mut()
                        .filter_map(|active| active.subscription.take())
                        .collect::<Vec<_>>();

                    (callback, initial_subscriptions)
                };

                TerminationAdmission::Owner(ServerTermination {
                    server: Arc::clone(self),
                    wait,
                    callback,
                    initial_subscriptions,
                })
            }
        }
    }

    fn terminate(self: &Arc<Self>) -> XllResult<()> {
        match self.begin_termination() {
            TerminationAdmission::Owner(owner) => {
                owner.request_cancel();
                owner.finish();
            }
            TerminationAdmission::Waiter(waiter) => {
                waiter.wait();
            }
            TerminationAdmission::Complete => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerTerminationPhase {
    Open,
    Terminating,
    Terminated,
}

struct TerminationCoordinator {
    state: Mutex<ServerTerminationPhase>,
    completed: Condvar,
}

impl Default for TerminationCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(ServerTerminationPhase::Open),
            completed: Condvar::new(),
        }
    }
}

enum TerminationAdmission<'a> {
    Owner(ServerTermination<'a>),
    Waiter(ServerTerminationWaiter<'a>),
    Complete,
}

struct ServerTerminationWaiter<'a> {
    coordinator: &'a TerminationCoordinator,
}

impl<'a> ServerTerminationWaiter<'a> {
    fn wait(self) {
        let mut state = self.coordinator.state.lock();
        while *state != ServerTerminationPhase::Terminated {
            self.coordinator.completed.wait(&mut state);
        }
    }
}

struct TerminatedTopic {
    key: SubscriptionKey,
    generation: ConnectionGeneration,
    subscription: Option<Box<dyn RtdSubscription>>,
}

struct ServerTermination<'a> {
    server: Arc<ServerRuntime>,
    wait: TerminationWaitGuard<'a>,
    callback: Option<NotificationCallback>,
    initial_subscriptions: Vec<Box<dyn RtdSubscription>>,
}

impl<'a> ServerTermination<'a> {
    fn request_cancel(&self) {
        for sub in &self.initial_subscriptions {
            let _ = catch_unwind(AssertUnwindSafe(|| sub.request_cancel()));
        }
    }

    fn finish(mut self) {
        drop(self.callback);
        self.wait.wait();

        let (late_callback, active_entries) = {
            let mut state = self.server.state.lock();
            let late_callback = state.delivery.detach_callback();
            state.delivery.updates.clear();

            let active_entries = state
                .active_by_topic
                .drain()
                .map(|(_, active)| TerminatedTopic {
                    key: active.key,
                    generation: active.generation,
                    subscription: active.subscription,
                })
                .collect::<Vec<_>>();

            state.topic_by_key.clear();
            state.lifecycle = ServerLifecycle::Terminated;

            (late_callback, active_entries)
        };
        drop(late_callback);

        let removed_sources = if let Some(parent) = self.server.parent.upgrade() {
            let mut catalog = parent.catalog.lock();
            let mut sources = Vec::new();

            for topic in &active_entries {
                if let Some(src) = cleanup_catalog_binding_and_pending(
                    &mut catalog,
                    &topic.key,
                    self.server.generation,
                    topic.generation,
                ) {
                    sources.push(src);
                }
            }

            sources
        } else {
            Vec::new()
        };

        if let Some(parent) = self.server.parent.upgrade() {
            let mut catalog = parent.catalog.lock();
            let unactive_pending_keys: Vec<_> = catalog
                .pending
                .iter()
                .filter(|(_, p)| p.server_generation == Some(self.server.generation))
                .map(|(k, _)| k.clone())
                .collect();

            let mut extra_sources = Vec::new();
            for key in unactive_pending_keys {
                let should_remove = catalog.pending.get_mut(&key).is_some_and(|pending| {
                    pending.server_generation = None;
                    pending.connecting_generation = None;
                    pending.committed = false;
                    pending.live_reservations == 0
                });

                if should_remove {
                    let Some(removed) = catalog.pending.remove(&key) else {
                        continue;
                    };
                    catalog.pending_topic_bytes = catalog
                        .pending_topic_bytes
                        .saturating_sub(removed.topic.byte_len());
                    extra_sources.push(removed.source);
                }
            }
            drop(catalog);
            for src in extra_sources {
                let _ = catch_unwind(AssertUnwindSafe(|| drop(src)));
            }
        }

        for source in removed_sources {
            let _ = catch_unwind(AssertUnwindSafe(|| drop(source)));
        }

        let all_subscriptions = self
            .initial_subscriptions
            .drain(..)
            .chain(active_entries.into_iter().filter_map(|e| e.subscription));
        let cleanup_result = disconnect_all_no_unwind(all_subscriptions);

        if let Some(parent) = self.server.parent.upgrade() {
            parent.record_cleanup_result(cleanup_result);
        }

        self.server.remove_from_registry();

        let mut term_state = self.server.termination_coordinator.state.lock();
        *term_state = ServerTerminationPhase::Terminated;
        self.server.termination_coordinator.completed.notify_all();
    }
}

#[must_use]
pub(crate) struct RtdRefreshBatch {
    server: Arc<ServerRuntime>,
    operation: Option<ServerOperation>,
    refresh_id: u64,
    pub(crate) updates: Vec<RtdUpdate>,
    completed: bool,
}

impl RtdRefreshBatch {
    pub(crate) fn complete(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        let attempt =
            self.server
                .complete_refresh_inner(self.refresh_id, &self.updates, outcome)?;
        if let Some(attempt) = attempt {
            self.server.drive_notification(attempt);
        }
        self.completed = true;
        self.operation.take();
        Ok(())
    }
}

impl Drop for RtdRefreshBatch {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.server.abort_refresh_no_unwind(self.refresh_id);
    }
}

fn disconnect_all_no_unwind(
    subscriptions: impl IntoIterator<Item = Box<dyn RtdSubscription>>,
) -> XllResult<()> {
    let mut first_error = None;
    for subscription in subscriptions {
        let res = catch_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait()));
        match res {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
            Err(_) => {
                if first_error.is_none() {
                    first_error = Some(XllError::Internal {
                        diagnostic_id: 0x5041_4e49_4344_4953,
                    });
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn cleanup_catalog_binding_and_pending(
    catalog: &mut SubscriptionCatalog,
    key: &SubscriptionKey,
    server_generation: ServerGeneration,
    conn_generation: ConnectionGeneration,
) -> Option<Arc<dyn ErasedRtdSource>> {
    if catalog
        .active_keys
        .get(key)
        .is_some_and(|binding| binding.connection_generation == conn_generation)
    {
        catalog.active_keys.remove(key);
    }

    if let Some(pending) = catalog
        .pending
        .get_mut(key)
        .filter(|p| p.server_generation == Some(server_generation))
    {
        if pending.connecting_generation == Some(conn_generation) {
            pending.connecting_generation = None;
        }
        pending.server_generation = None;
        pending.committed = false;
    }

    if let Some(pending) = catalog.pending.get(key).filter(|p| {
        p.connecting_generation.is_none()
            && p.server_generation.is_none()
            && p.live_reservations == 0
    }) {
        let _ = pending;
        let removed = catalog.pending.remove(key);
        if let Some(removed) = removed {
            catalog.pending_topic_bytes = catalog
                .pending_topic_bytes
                .saturating_sub(removed.topic.byte_len());
            return Some(removed.source);
        }
    }

    None
}

enum ServerReservationFailure {
    DuplicateTopicId,
    DuplicateKey,
    Overloaded(XllError),
}

impl ServerReservationFailure {
    fn into_xll_error(self) -> XllError {
        match self {
            ServerReservationFailure::DuplicateTopicId => XllError::Internal {
                diagnostic_id: 0x544f_5049_4349_4444,
            },
            ServerReservationFailure::DuplicateKey => XllError::Internal {
                diagnostic_id: 0x544f_5049_434b_4559,
            },
            ServerReservationFailure::Overloaded(err) => err,
        }
    }
}

#[cfg(test)]
type OperationEnterHook = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct SubscriptionRuntime {
    limits: RtdLimits,
    module_ingress: Option<&'static crate::ingress::ExportIngress>,
    runtime_gate: Arc<OperationGate>,
    catalog: Mutex<SubscriptionCatalog>,
    servers: Mutex<HashMap<ServerGeneration, Arc<ServerRuntime>>>,
    active_quota: Arc<Quota>,
    queued_update_quota: Arc<Quota>,
    cleanup_failure: Mutex<Option<XllError>>,
    next_preparation_id: AtomicU64,
    next_connection_generation: AtomicU64,
    termination_coordinator: TerminationCoordinator,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    test_enter_hook: Mutex<Option<OperationEnterHook>>,
}

impl SubscriptionRuntime {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_limits(RtdLimits::standard())
    }

    #[cfg(test)]
    pub(crate) fn with_limits(limits: RtdLimits) -> Self {
        Self::with_limits_and_ingress(limits, None)
    }

    pub(crate) fn with_module_ingress(limits: RtdLimits) -> Self {
        Self::with_limits_and_ingress(limits, Some(crate::ingress::global_ingress()))
    }

    fn with_limits_and_ingress(
        limits: RtdLimits,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> Self {
        Self {
            limits,
            module_ingress,
            runtime_gate: OperationGate::new(),
            catalog: Mutex::new(SubscriptionCatalog {
                pending: HashMap::new(),
                pending_topic_bytes: 0,
                source_ids: HashMap::new(),
                active_keys: HashMap::new(),
            }),
            servers: Mutex::new(HashMap::new()),
            active_quota: Arc::new(Quota::new(limits.max_active)),
            queued_update_quota: Arc::new(Quota::new(limits.max_queued_updates)),
            cleanup_failure: Mutex::new(None),
            next_preparation_id: AtomicU64::new(1),
            next_connection_generation: AtomicU64::new(1),
            termination_coordinator: TerminationCoordinator::default(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            test_enter_hook: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_operation_enter_hook(&self, hook: Option<OperationEnterHook>) {
        *self.test_enter_hook.lock() = hook;
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
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

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup_failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    pub(crate) fn enter_external_operation(&self) -> XllResult<OperationGuard> {
        self.runtime_gate.enter()
    }

    pub(crate) fn register_server(
        self: &Arc<Self>,
        generation: ServerGeneration,
    ) -> XllResult<RtdServerHandle> {
        let _operation = self.runtime_gate.enter()?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        let server = Arc::new(ServerRuntime {
            generation,
            module_ingress: self.module_ingress,
            operation_gate: OperationGate::new(),
            state: Mutex::new(ServerState {
                lifecycle: ServerLifecycle::Open,
                active_by_topic: HashMap::new(),
                topic_by_key: HashMap::new(),
                delivery: ServerDeliveryState::default(),
            }),
            parent: Arc::downgrade(self),
            termination_coordinator: TerminationCoordinator::default(),
        });

        let mut servers = self.servers.lock();
        if servers.contains_key(&generation) {
            return Err(XllError::Internal {
                diagnostic_id: 0x5254_4453_5256_4455,
            });
        }
        servers.insert(generation, Arc::clone(&server));
        Ok(RtdServerHandle { inner: server })
    }

    pub(crate) fn prepare<S>(
        self: &Arc<Self>,
        source: S,
        topic: RtdTopic,
    ) -> XllResult<PreparedSubscription>
    where
        S: RtdSource,
    {
        let _operation = self.runtime_gate.enter()?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        topic.validate_with_limits(&self.limits)?;

        let source = Arc::new(source);
        let ptr_key = Arc::as_ptr(&source) as usize;

        let key = {
            let mut catalog = self.catalog.lock();
            let source_id = if let Some(&id) = catalog.source_ids.get(&ptr_key) {
                id
            } else {
                if catalog.source_ids.len() >= self.limits.max_source_ids {
                    return Err(XllError::Overloaded);
                }
                let id = catalog.source_ids.len() as u64 + 1;
                catalog.source_ids.insert(ptr_key, id);
                id
            };

            let mut parts_str = String::new();
            for part in topic.parts() {
                parts_str.push_str(part);
                parts_str.push('\0');
            }
            format!("{source_id:016x}:{parts_str}")
        };

        let key = SubscriptionKey::new(key);

        let mut catalog = self.catalog.lock();
        if catalog.active_keys.contains_key(&key) {
            return Ok(PreparedSubscription {
                runtime: Arc::downgrade(self),
                key,
                reservation_id: None,
                ownership: PreparationOwnership::ExistingActive,
            });
        }

        if let Some(pending) = catalog.pending.get_mut(&key) {
            let reservation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);
            pending.live_reservations =
                pending
                    .live_reservations
                    .checked_add(1)
                    .ok_or(XllError::Internal {
                        diagnostic_id: 0x5245_5356_4f56_464c,
                    })?;
            return Ok(PreparedSubscription {
                runtime: Arc::downgrade(self),
                key,
                reservation_id: Some(reservation_id),
                ownership: PreparationOwnership::ExistingPending,
            });
        }

        if catalog.pending.len() >= self.limits.max_pending {
            return Err(XllError::Overloaded);
        }

        let new_total = catalog
            .pending_topic_bytes
            .checked_add(topic.byte_len())
            .ok_or(XllError::Overloaded)?;
        if new_total > self.limits.max_total_topic_bytes {
            return Err(XllError::Overloaded);
        }

        let reservation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);
        catalog.pending_topic_bytes = new_total;
        catalog.pending.insert(
            key.clone(),
            PendingSubscription {
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
            reservation_id: Some(reservation_id),
            ownership: PreparationOwnership::CreatedPending,
        })
    }

    fn finish_preparation(&self, key: &SubscriptionKey, _reservation_id: u64, committed: bool) {
        let (removed_source, _removed_topic_bytes) = {
            let mut catalog = self.catalog.lock();
            let Some(pending) = catalog.pending.get_mut(key) else {
                return;
            };

            pending.live_reservations = pending.live_reservations.saturating_sub(1);
            if committed {
                pending.committed = true;
                return;
            }

            if pending.live_reservations == 0
                && !pending.committed
                && pending.connecting_generation.is_none()
            {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    (Some(removed.source), bytes)
                } else {
                    (None, 0)
                }
            } else {
                (None, 0)
            }
        };

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5243,
                }));
            }
        }
    }

    pub(crate) fn claim_server_key(
        &self,
        generation: ServerGeneration,
        key: &SubscriptionKey,
    ) -> XllResult<()> {
        let mut catalog = self.catalog.lock();
        let pending = catalog.pending.get_mut(key).ok_or(XllError::Closing)?;

        if let Some(existing_gen) = pending.server_generation {
            if existing_gen != generation {
                return Err(XllError::Internal {
                    diagnostic_id: 0x5352_5647_454e_4d49,
                });
            }
        } else {
            pending.server_generation = Some(generation);
        }
        Ok(())
    }

    fn rollback_catalog_connection_reservation(
        &self,
        key: &SubscriptionKey,
        generation: ConnectionGeneration,
    ) {
        let mut catalog = self.catalog.lock();

        if let Some(pending) = catalog
            .pending
            .get_mut(key)
            .filter(|p| p.connecting_generation == Some(generation))
        {
            pending.connecting_generation = None;
        }

        if catalog
            .active_keys
            .get(key)
            .is_some_and(|binding| binding.connection_generation == generation)
        {
            catalog.active_keys.remove(key);
        }
    }

    pub(crate) fn connect_transaction(
        self: &Arc<Self>,
        server_handle: &RtdServerHandle,
        topic_id: TopicId,
        key: &SubscriptionKey,
    ) -> XllResult<SubscriptionConnection> {
        let operation = server_handle.inner.enter_operation()?;
        let conn_gen = ConnectionGeneration(
            self.next_connection_generation
                .fetch_add(1, Ordering::Relaxed),
        );

        let (source, topic) = {
            let mut catalog = self.catalog.lock();

            if catalog.active_keys.contains_key(key) {
                return Err(XllError::Internal {
                    diagnostic_id: 0x4143_5456_4b45_5944,
                });
            }

            let (source, topic) = {
                let pending = catalog.pending.get_mut(key).ok_or(XllError::Closing)?;

                if let Some(existing_gen) = pending.server_generation {
                    if existing_gen != server_handle.inner.generation {
                        return Err(XllError::Internal {
                            diagnostic_id: 0x5352_5647_454e_4d49,
                        });
                    }
                } else {
                    pending.server_generation = Some(server_handle.inner.generation);
                }

                if pending.connecting_generation.is_some() {
                    return Err(XllError::Internal {
                        diagnostic_id: 0x434f_4e4e_494e_464c,
                    });
                }
                pending.connecting_generation = Some(conn_gen);
                (Arc::clone(&pending.source), pending.topic.clone())
            };

            catalog.active_keys.insert(
                key.clone(),
                ActiveKeyBinding {
                    connection_generation: conn_gen,
                    stage: BindingStage::Connecting,
                },
            );

            (source, topic)
        };

        let reservation_result = {
            let mut state = server_handle.inner.state.lock();

            if let Err(err) = state.ensure_open() {
                Err(ServerReservationFailure::Overloaded(err))
            } else if state.active_by_topic.contains_key(&topic_id) {
                Err(ServerReservationFailure::DuplicateTopicId)
            } else if state.topic_by_key.contains_key(key) {
                Err(ServerReservationFailure::DuplicateKey)
            } else {
                match self.active_quota.try_acquire() {
                    Ok(permit) => {
                        state.topic_by_key.insert(key.clone(), topic_id);
                        state.active_by_topic.insert(
                            topic_id,
                            ActiveSubscription {
                                key: key.clone(),
                                generation: conn_gen,
                                subscription: None,
                                committed: false,
                                latest: RtdValue::Empty,
                                _permit: permit,
                            },
                        );
                        Ok(())
                    }
                    Err(error) => Err(ServerReservationFailure::Overloaded(error)),
                }
            }
        };

        if let Err(failure) = reservation_result {
            self.rollback_catalog_connection_reservation(key, conn_gen);
            return Err(failure.into_xll_error());
        }

        let erased_sink = ErasedSink {
            server: Arc::downgrade(&server_handle.inner),
            topic_id,
            connection_generation: conn_gen,
        };

        let sub_res = catch_unwind(AssertUnwindSafe(|| source.subscribe(&topic, erased_sink)));

        let subscription = match sub_res {
            Ok(Ok(sub)) => sub,
            Ok(Err(err)) => {
                self.rollback_connection(server_handle, topic_id, conn_gen, key);
                return Err(err);
            }
            Err(panic_payload) => {
                self.rollback_connection(server_handle, topic_id, conn_gen, key);
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5542,
                }));
                std::panic::resume_unwind(panic_payload);
            }
        };

        let install_result = {
            let mut state = server_handle.inner.state.lock();
            if state.ensure_open().is_err() {
                Err(subscription)
            } else {
                match state.active_by_topic.get_mut(&topic_id) {
                    Some(active) if active.generation == conn_gen => {
                        active.subscription = Some(subscription);
                        let latest = active.latest.clone();
                        let observed = state
                            .delivery
                            .updates
                            .get(&topic_id)
                            .filter(|u| u.connection_generation == conn_gen)
                            .map(|u| u.sequence);
                        Ok((latest, observed))
                    }
                    _ => Err(subscription),
                }
            }
        };

        let (latest_value, observed_sequence) = match install_result {
            Ok(res) => res,
            Err(sub) => {
                let _ = catch_unwind(AssertUnwindSafe(|| sub.disconnect_and_wait()));
                self.rollback_connection(server_handle, topic_id, conn_gen, key);
                return Err(XllError::Closing);
            }
        };

        Ok(SubscriptionConnection {
            runtime: Arc::clone(self),
            server: Arc::clone(&server_handle.inner),
            operation: Some(operation),
            topic_id,
            generation: conn_gen,
            key: key.clone(),
            value: latest_value,
            observed_sequence,
            created: true,
            finished: false,
        })
    }

    fn commit_connection(
        &self,
        server: &Arc<ServerRuntime>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        key: &SubscriptionKey,
        observed_sequence: Option<u64>,
    ) -> XllResult<()> {
        let attempt = {
            let mut state = server.state.lock();
            state.ensure_open()?;
            let Some(active) = state.active_by_topic.get_mut(&topic_id) else {
                return Err(XllError::Closing);
            };
            if active.generation != generation {
                return Err(XllError::Closing);
            }
            active.committed = true;

            if let Some(obs) = observed_sequence {
                state
                    .delivery
                    .updates
                    .retain(|&tid, u| tid != topic_id || u.sequence > obs);
            }

            let has_updates = state.has_deliverable_updates();
            let prepared = state.delivery.prepare_notification(has_updates)?;
            prepared.map(|p| state.delivery.commit_notification(p))
        };

        let removed_source = {
            let mut catalog = self.catalog.lock();
            if let Some(binding) = catalog
                .active_keys
                .get_mut(key)
                .filter(|b| b.connection_generation == generation)
            {
                binding.stage = BindingStage::Active;
            }
            if let Some(pending) = catalog
                .pending
                .get_mut(key)
                .filter(|p| p.connecting_generation == Some(generation))
            {
                pending.connecting_generation = None;
            }
            let remove_pending = catalog
                .pending
                .get(key)
                .is_some_and(|p| p.live_reservations == 0 && p.connecting_generation.is_none());

            if remove_pending {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    Some(removed.source)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5243,
                }));
            }
        }

        if let Some(attempt) = attempt {
            server.drive_notification(attempt);
        }

        Ok(())
    }

    fn rollback_connection(
        &self,
        server_handle: &RtdServerHandle,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        key: &SubscriptionKey,
    ) {
        let (subscription, _removed_update) = {
            let mut state = server_handle.inner.state.lock();
            let sub = state
                .active_by_topic
                .get_mut(&topic_id)
                .filter(|a| a.generation == generation)
                .and_then(|a| a.subscription.take());

            if state
                .active_by_topic
                .get(&topic_id)
                .is_some_and(|a| a.generation == generation)
            {
                state.active_by_topic.remove(&topic_id);
            }
            if state.topic_by_key.get(key).is_some_and(|&tid| {
                state
                    .active_by_topic
                    .get(&tid)
                    .is_none_or(|a| a.generation == generation)
            }) {
                state.topic_by_key.remove(key);
            }

            let rem_update = if state
                .delivery
                .updates
                .get(&topic_id)
                .is_some_and(|u| u.connection_generation == generation)
            {
                state.delivery.updates.remove(&topic_id)
            } else {
                None
            };
            (sub, rem_update)
        };

        let removed_source = {
            let mut catalog = self.catalog.lock();
            if catalog
                .active_keys
                .get(key)
                .is_some_and(|b| b.connection_generation == generation)
            {
                catalog.active_keys.remove(key);
            }

            if let Some(pending) = catalog
                .pending
                .get_mut(key)
                .filter(|p| p.connecting_generation == Some(generation))
            {
                pending.connecting_generation = None;
            }

            let remove_pending = catalog.pending.get(key).is_some_and(|p| {
                p.live_reservations == 0 && !p.committed && p.connecting_generation.is_none()
            });

            if remove_pending {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    Some(removed.source)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(sub) = subscription {
            let res = catch_unwind(AssertUnwindSafe(|| sub.disconnect_and_wait()));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4344_4953,
                }));
            }
        }

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5243,
                }));
            }
        }
    }

    pub(crate) fn disconnect(
        &self,
        server_handle: &RtdServerHandle,
        topic_id: TopicId,
    ) -> XllResult<()> {
        let (subscription, key_to_clean, conn_gen) = {
            let mut state = server_handle.inner.state.lock();
            state.ensure_open()?;
            let Some((tid, active)) = state.active_by_topic.remove_entry(&topic_id) else {
                return Ok(());
            };
            state.topic_by_key.remove(&active.key);
            state.delivery.updates.remove(&tid);
            (active.subscription, active.key, active.generation)
        };

        let removed_source = {
            let mut catalog = self.catalog.lock();
            cleanup_catalog_binding_and_pending(
                &mut catalog,
                &key_to_clean,
                server_handle.inner.generation,
                conn_gen,
            )
        };

        if let Some(sub) = subscription {
            let res = catch_unwind(AssertUnwindSafe(|| sub.disconnect_and_wait()));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4344_4953,
                }));
            }
        }

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5243,
                }));
            }
        }

        Ok(())
    }

    pub(crate) fn close(&self) -> XllResult<()> {
        {
            let mut term_state = self.termination_coordinator.state.lock();
            match *term_state {
                ServerTerminationPhase::Terminated => return self.cleanup_result(),
                ServerTerminationPhase::Terminating => {
                    while *term_state != ServerTerminationPhase::Terminated {
                        self.termination_coordinator.completed.wait(&mut term_state);
                    }
                    return self.cleanup_result();
                }
                ServerTerminationPhase::Open => {
                    *term_state = ServerTerminationPhase::Terminating;
                }
            }
        }

        let runtime_wait = self.runtime_gate.close_and_wait_begin();
        runtime_wait.wait();

        let server_handles = {
            let servers = self.servers.lock();
            servers.values().cloned().collect::<Vec<_>>()
        };

        let admissions = server_handles
            .iter()
            .map(ServerRuntime::begin_termination)
            .collect::<Vec<_>>();

        for admission in &admissions {
            if let TerminationAdmission::Owner(owner) = admission {
                owner.request_cancel();
            }
        }

        for admission in admissions {
            match admission {
                TerminationAdmission::Owner(owner) => owner.finish(),
                TerminationAdmission::Waiter(waiter) => waiter.wait(),
                TerminationAdmission::Complete => {}
            }
        }

        let pending_sources = {
            let mut catalog = self.catalog.lock();
            catalog.active_keys.clear();
            catalog.source_ids.clear();
            catalog.pending_topic_bytes = 0;
            catalog
                .pending
                .drain()
                .map(|(_, pending)| pending.source)
                .collect::<Vec<_>>()
        };

        for source in pending_sources {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: 0x5041_4e49_4353_5243,
                }));
            }
        }

        {
            let mut term_state = self.termination_coordinator.state.lock();
            *term_state = ServerTerminationPhase::Terminated;
            self.termination_coordinator.completed.notify_all();
        }

        self.cleanup_result()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparationOwnership {
    CreatedPending,
    ExistingPending,
    ExistingActive,
}

pub(crate) struct PreparedSubscription {
    runtime: Weak<SubscriptionRuntime>,
    key: SubscriptionKey,
    reservation_id: Option<u64>,
    ownership: PreparationOwnership,
}

impl PreparedSubscription {
    pub(crate) fn key(&self) -> &str {
        &self.key
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

pub(crate) struct SubscriptionConnection {
    runtime: Arc<SubscriptionRuntime>,
    server: Arc<ServerRuntime>,
    operation: Option<ServerOperation>,
    topic_id: TopicId,
    generation: ConnectionGeneration,
    key: SubscriptionKey,
    value: RtdValue,
    observed_sequence: Option<u64>,
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
        let result = if self.created {
            self.runtime.commit_connection(
                &self.server,
                self.topic_id,
                self.generation,
                &self.key,
                self.observed_sequence,
            )
        } else {
            Ok(())
        };

        if result.is_ok() {
            self.finished = true;
            self.operation.take();
        }
        result
    }

    fn rollback(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created {
            let handle = RtdServerHandle {
                inner: Arc::clone(&self.server),
            };
            self.runtime
                .rollback_connection(&handle, self.topic_id, self.generation, &self.key);
        }
        self.operation.take();
    }
}

impl Drop for SubscriptionConnection {
    fn drop(&mut self) {
        self.rollback();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct TestSubscription {
        canceled: Arc<AtomicBool>,
        disconnected: Arc<AtomicBool>,
    }

    // SAFETY: TestSubscription performs no background thread work.
    unsafe impl RtdSubscription for TestSubscription {
        fn request_cancel(&self) {
            self.canceled.store(true, Ordering::Release);
        }

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.disconnected.store(true, Ordering::Release);
            Ok(())
        }
    }

    pub(crate) struct PublishingSource<T = RtdValue, F = fn() -> XllResult<()>> {
        initial: Option<T>,
        sink_slot: Arc<Mutex<Option<RtdSink<T>>>>,
        canceled: Arc<AtomicBool>,
        disconnected: Arc<AtomicBool>,
        on_subscribe: Option<F>,
    }

    impl<T, F> RtdSource for PublishingSource<T, F>
    where
        T: IntoRtdValue + Clone + Send + Sync + 'static,
        F: Fn() -> XllResult<()> + Send + Sync + 'static,
    {
        type Value = T;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            if let Some(on_sub) = &self.on_subscribe {
                on_sub()?;
            }
            if let Some(initial) = self.initial.clone() {
                sink.publish(initial)?;
            }
            *self.sink_slot.lock() = Some(sink);
            Ok(Box::new(TestSubscription {
                canceled: Arc::clone(&self.canceled),
                disconnected: Arc::clone(&self.disconnected),
            }))
        }
    }

    pub(crate) type PublishingSourceResult<T> = (
        PublishingSource<T, fn() -> XllResult<()>>,
        Arc<Mutex<Option<RtdSink<T>>>>,
        Arc<AtomicBool>,
    );

    pub(crate) fn publishing_source<T: IntoRtdValue + Clone + Send + Sync + 'static>(
        initial: Option<T>,
    ) -> PublishingSourceResult<T> {
        let slot = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = PublishingSource {
            initial,
            sink_slot: Arc::clone(&slot),
            canceled: Arc::new(AtomicBool::new(false)),
            disconnected: Arc::clone(&disconnected),
            on_subscribe: None,
        };
        (source, slot, disconnected)
    }

    #[test]
    fn server_publish_isolation() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
        let (source_b, sink_b, _) = publishing_source(Some(2.0f64));

        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a").unwrap())
            .unwrap();
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b").unwrap())
            .unwrap();

        let key_a = SubscriptionKey::new(prep_a.key());
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_a.commit();
        prep_b.commit();

        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let _sink_a = sink_a.lock().clone().unwrap();
        let sink_b = sink_b.lock().clone().unwrap();

        let lock_guard = server_a.inner.state.lock();

        let b_published = Arc::new(AtomicBool::new(false));
        let b_published_clone = Arc::clone(&b_published);
        let handle_b = std::thread::spawn(move || {
            sink_b.publish(100.0).unwrap();
            b_published_clone.store(true, Ordering::Release);
        });

        handle_b.join().unwrap();
        assert!(b_published.load(Ordering::Acquire));
        drop(lock_guard);
    }

    #[test]
    fn notification_callback_isolation() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));

        server_a
            .attach_update_callback(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.lock().recv().unwrap();
                Ok(())
            }))
            .unwrap();

        let callback_b_count = Arc::new(AtomicUsize::new(0));
        let cb_b = Arc::clone(&callback_b_count);
        server_b
            .attach_update_callback(Arc::new(move || {
                cb_b.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .unwrap();

        let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
        let (source_b, sink_b, _) = publishing_source(Some(2.0f64));

        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a").unwrap())
            .unwrap();
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b").unwrap())
            .unwrap();
        let key_a = SubscriptionKey::new(prep_a.key());
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_a.commit();
        prep_b.commit();

        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let sink_a = sink_a.lock().clone().unwrap();
        let sink_b = sink_b.lock().clone().unwrap();

        let thread_a = std::thread::spawn(move || {
            sink_a.publish(10.0).unwrap();
        });

        entered_rx.recv().unwrap();

        for i in 0..100 {
            sink_b.publish(i as f64).unwrap();
        }

        assert!(callback_b_count.load(Ordering::SeqCst) > 0);

        release_tx.send(()).unwrap();
        thread_a.join().unwrap();
    }

    #[test]
    fn server_locality_refresh_lock_independence() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b-0").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let sink_b = sink_b.lock().clone().unwrap();
        sink_b.publish(42.0).unwrap();

        // server A の state mutex を保持した状態で server B.begin_refresh を実行
        let _guard_a = server_a.inner.state.lock();

        let (tx, rx) = std::sync::mpsc::channel();
        let server_b_clone = server_b.clone();
        std::thread::spawn(move || {
            let batch = server_b_clone.begin_refresh().unwrap();
            tx.send(batch).unwrap();
        });

        let batch = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("server_b.begin_refresh should not block on server_a state lock");

        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, RtdValue::Number(42.0));
        batch.complete(RefreshOutcome::Delivered).unwrap();
    }

    #[test]
    fn runtime_close_blocks_all_servers_immediately() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b-0").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let (callback_started_tx, callback_started_rx) = std::sync::mpsc::channel();
        let (unblock_callback_tx, unblock_callback_rx) = std::sync::mpsc::channel();
        let unblock_callback_rx = Arc::new(Mutex::new(unblock_callback_rx));

        let unblock_rx_clone = Arc::clone(&unblock_callback_rx);
        server_a
            .attach_update_callback(Arc::new(move || {
                callback_started_tx.send(()).unwrap();
                let _ = unblock_rx_clone.lock().recv();
                Ok(())
            }))
            .unwrap();

        let (source_a, sink_a, _) = publishing_source(Some(0.0f64));
        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a-0").unwrap())
            .unwrap();
        let key_a = SubscriptionKey::new(prep_a.key());
        prep_a.commit();
        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();

        let sink_a = sink_a.lock().clone().unwrap();
        let publish_handle = std::thread::spawn(move || {
            sink_a.publish(1.0).unwrap();
        });

        callback_started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();

        let runtime_clone = Arc::clone(&runtime);
        let close_handle = std::thread::spawn(move || {
            runtime_clone.close().unwrap();
        });

        // server_a が close 処理に入り OperationGate が Closing になるまで待機
        while server_a.inner.enter_operation().is_ok() {
            std::thread::yield_now();
        }

        let sink_b = sink_b.lock().clone().unwrap();
        assert!(matches!(sink_b.publish(42.0), Err(XllError::Closing)));

        unblock_callback_tx.send(()).unwrap();
        publish_handle.join().unwrap();
        close_handle.join().unwrap();
    }

    #[test]
    fn server_termination_clears_pending_and_allows_reconnect() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();

        let (source, sink, disconnected) = publishing_source(Some(10.0));
        let source = Arc::new(source);
        let prep = runtime
            .prepare(source.clone(), RtdTopic::single("shared-topic").unwrap())
            .unwrap();
        let key = SubscriptionKey::new(prep.key());
        prep.commit();

        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key)
            .unwrap();
        conn_a.commit().unwrap();

        // server A を終了
        server_a.terminate().unwrap();

        // disconnect が呼ばれていること
        assert!(disconnected.load(Ordering::SeqCst));
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();
        let prep_b = runtime
            .prepare(source, RtdTopic::single("shared-topic").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();

        runtime
            .claim_server_key(server_b.inner.generation, &key_b)
            .unwrap();

        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(20.0).unwrap();

        let batch = server_b.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, RtdValue::Number(20.0));
        batch.complete(RefreshOutcome::Delivered).unwrap();
    }

    #[test]
    fn uncommitted_update_does_not_trigger_notification() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server = runtime.register_server(ServerGeneration(1)).unwrap();

        let callback_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&callback_count);
        server
            .attach_update_callback(Arc::new(move || {
                count_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .unwrap();

        let (source_a, _, _) = publishing_source(Some(0.0f64));
        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a-0").unwrap())
            .unwrap();
        let key_a = SubscriptionKey::new(prep_a.key());
        prep_a.commit();
        let conn_a = runtime
            .connect_transaction(&server, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();

        let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b-0").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();
        let _conn_b = runtime
            .connect_transaction(&server, TopicId(2), &key_b)
            .unwrap();

        callback_count.store(0, Ordering::SeqCst);

        let sink_b = sink_b.lock().clone().unwrap();
        sink_b.publish(100.0).unwrap();

        server.pulse_notification().unwrap();

        assert_eq!(callback_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn server_standalone_termination() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let sink_b = sink_b.lock().clone().unwrap();

        let server_a_clone = server_a.clone();
        let handle_term = std::thread::spawn(move || {
            server_a_clone.terminate().unwrap();
        });

        sink_b.publish(1.0).unwrap();
        let batch = server_b.begin_refresh().unwrap();
        batch.complete(RefreshOutcome::Delivered).unwrap();

        handle_term.join().unwrap();
    }

    #[test]
    fn stale_sink_returns_closing() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source_a, sink_a, _) = publishing_source(Some(0.0f64));
        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a").unwrap())
            .unwrap();
        let key_a = SubscriptionKey::new(prep_a.key());
        prep_a.commit();
        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();

        let sink_a = sink_a.lock().clone().unwrap();

        server_a.terminate().unwrap();

        assert!(matches!(sink_a.publish(1.0), Err(XllError::Closing)));

        let (source_b, sink_b, _) = publishing_source(Some(0.0f64));
        let prep_b = runtime
            .prepare(source_b, RtdTopic::single("b").unwrap())
            .unwrap();
        let key_b = SubscriptionKey::new(prep_b.key());
        prep_b.commit();
        let conn_b = runtime
            .connect_transaction(&server_b, TopicId(1), &key_b)
            .unwrap();
        conn_b.commit().unwrap();

        let sink_b = sink_b.lock().clone().unwrap();
        assert!(sink_b.publish(2.0).is_ok());
    }

    #[test]
    fn global_quota_enforcement() {
        let limits = RtdLimits {
            max_queued_updates: 10,
            ..RtdLimits::standard()
        };
        let runtime = Arc::new(SubscriptionRuntime::with_limits(limits));
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let mut sinks_a = Vec::new();
        for i in 0..6 {
            let (source, sink, _) = publishing_source(Some(0.0f64));
            let prep = runtime
                .prepare(source, RtdTopic::single(format!("a-{}", i)).unwrap())
                .unwrap();
            let key = SubscriptionKey::new(prep.key());
            prep.commit();
            let conn = runtime
                .connect_transaction(&server_a, TopicId(i), &key)
                .unwrap();
            conn.commit().unwrap();
            sinks_a.push(sink.lock().clone().unwrap());
        }

        let mut sinks_b = Vec::new();
        for i in 0..5 {
            let (source, sink, _) = publishing_source(Some(0.0f64));
            let prep = runtime
                .prepare(source, RtdTopic::single(format!("b-{}", i)).unwrap())
                .unwrap();
            let key = SubscriptionKey::new(prep.key());
            prep.commit();
            let conn = runtime
                .connect_transaction(&server_b, TopicId(i), &key)
                .unwrap();
            conn.commit().unwrap();
            sinks_b.push(sink.lock().clone().unwrap());
        }

        for sink in &sinks_a[0..6] {
            sink.publish(1.0).unwrap();
        }
        for sink in &sinks_b[0..4] {
            sink.publish(1.0).unwrap();
        }

        assert!(matches!(sinks_b[4].publish(1.0), Err(XllError::Overloaded)));

        let batch_a = server_a.begin_refresh().unwrap();
        batch_a.complete(RefreshOutcome::Delivered).unwrap();

        assert!(sinks_b[4].publish(1.0).is_ok());
    }

    #[test]
    fn key_binding_concurrency_rejection() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (source, _, _) = publishing_source(Some(1.0f64));
        let prep = runtime
            .prepare(source, RtdTopic::single("shared").unwrap())
            .unwrap();
        let key = SubscriptionKey::new(prep.key());
        prep.commit();

        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key)
            .unwrap();
        assert!(matches!(
            runtime.connect_transaction(&server_b, TopicId(1), &key),
            Err(XllError::Internal { .. })
        ));

        conn_a.commit().unwrap();
        assert!(matches!(
            runtime.connect_transaction(&server_b, TopicId(1), &key),
            Err(XllError::Internal { .. })
        ));
    }

    #[test]
    fn runtime_close_waits_for_inflight() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server_a = runtime.register_server(ServerGeneration(1)).unwrap();
        let _server_b = runtime.register_server(ServerGeneration(2)).unwrap();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));

        server_a
            .attach_update_callback(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx.lock().recv().unwrap();
                Ok(())
            }))
            .unwrap();

        let (source_a, sink_a, _) = publishing_source(Some(1.0f64));
        let prep_a = runtime
            .prepare(source_a, RtdTopic::single("a").unwrap())
            .unwrap();
        let key_a = SubscriptionKey::new(prep_a.key());
        prep_a.commit();
        let conn_a = runtime
            .connect_transaction(&server_a, TopicId(1), &key_a)
            .unwrap();
        conn_a.commit().unwrap();

        let sink_a = sink_a.lock().clone().unwrap();

        let handle_a = std::thread::spawn(move || {
            sink_a.publish(10.0).unwrap();
        });

        entered_rx.recv().unwrap();

        let runtime_clone = Arc::clone(&runtime);
        let closed_flag = Arc::new(AtomicBool::new(false));
        let cf = Arc::clone(&closed_flag);

        let handle_close = std::thread::spawn(move || {
            runtime_clone.close().unwrap();
            cf.store(true, Ordering::Release);
        });

        while (runtime.runtime_gate.state.load(Ordering::Acquire) & CLOSING_BIT) == 0 {
            std::thread::yield_now();
        }
        assert!(!closed_flag.load(Ordering::Acquire));

        release_tx.send(()).unwrap();
        handle_a.join().unwrap();
        handle_close.join().unwrap();
        assert!(closed_flag.load(Ordering::Acquire));
    }

    #[test]
    fn inflight_register_waits_for_close() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (enter_tx, enter_rx) = std::sync::mpsc::channel();
        let (unblock_tx, unblock_rx) = std::sync::mpsc::channel();
        let unblock_rx = Arc::new(Mutex::new(unblock_rx));

        runtime.set_operation_enter_hook(Some(Arc::new(move || {
            enter_tx.send(()).unwrap();
            unblock_rx.lock().recv().unwrap();
        })));

        let runtime_clone = Arc::clone(&runtime);
        let handle_reg =
            std::thread::spawn(move || runtime_clone.register_server(ServerGeneration(1)));

        enter_rx.recv().unwrap();

        let runtime_close = Arc::clone(&runtime);
        let closed_flag = Arc::new(AtomicBool::new(false));
        let cf = Arc::clone(&closed_flag);
        let handle_close = std::thread::spawn(move || {
            runtime_close.close().unwrap();
            cf.store(true, Ordering::Release);
        });

        while (runtime.runtime_gate.state.load(Ordering::Acquire) & CLOSING_BIT) == 0 {
            std::thread::yield_now();
        }
        assert!(!closed_flag.load(Ordering::Acquire));

        unblock_tx.send(()).unwrap();

        let reg_res = handle_reg.join().unwrap();
        handle_close.join().unwrap();

        assert!(closed_flag.load(Ordering::Acquire));
        assert!(reg_res.is_ok());
        assert!(runtime.servers.lock().is_empty());
    }

    #[test]
    fn inflight_prepare_waits_for_close() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (enter_tx, enter_rx) = std::sync::mpsc::channel();
        let (unblock_tx, unblock_rx) = std::sync::mpsc::channel();
        let unblock_rx = Arc::new(Mutex::new(unblock_rx));

        runtime.set_operation_enter_hook(Some(Arc::new(move || {
            enter_tx.send(()).unwrap();
            unblock_rx.lock().recv().unwrap();
        })));

        let source_dropped = Arc::new(AtomicBool::new(false));
        struct DroppingSource(Arc<AtomicBool>);
        impl Drop for DroppingSource {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        impl RtdSource for DroppingSource {
            type Value = RtdValue;
            fn subscribe(
                &self,
                _: &RtdTopic,
                _: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(TestSubscription {
                    canceled: Arc::new(AtomicBool::new(false)),
                    disconnected: Arc::new(AtomicBool::new(false)),
                }))
            }
        }

        let source = DroppingSource(Arc::clone(&source_dropped));
        let runtime_clone = Arc::clone(&runtime);
        let handle_prep = std::thread::spawn(move || {
            runtime_clone.prepare(source, RtdTopic::single("topic").unwrap())
        });

        enter_rx.recv().unwrap();

        let runtime_close = Arc::clone(&runtime);
        let closed_flag = Arc::new(AtomicBool::new(false));
        let cf = Arc::clone(&closed_flag);
        let handle_close = std::thread::spawn(move || {
            runtime_close.close().unwrap();
            cf.store(true, Ordering::Release);
        });

        while (runtime.runtime_gate.state.load(Ordering::Acquire) & CLOSING_BIT) == 0 {
            std::thread::yield_now();
        }
        assert!(!closed_flag.load(Ordering::Acquire));

        unblock_tx.send(()).unwrap();

        let prep_res = handle_prep.join().unwrap();
        handle_close.join().unwrap();

        assert!(closed_flag.load(Ordering::Acquire));
        let prep = prep_res.unwrap();
        drop(prep);

        let catalog = runtime.catalog.lock();
        assert!(catalog.pending.is_empty());
        drop(catalog);

        assert!(source_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn reentrant_drop_safety() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server = runtime.register_server(ServerGeneration(1)).unwrap();

        struct ReentrantSource {
            runtime: Arc<SubscriptionRuntime>,
        }

        impl Drop for ReentrantSource {
            fn drop(&mut self) {
                let _ = self.runtime.cleanup_result();
            }
        }

        impl RtdSource for ReentrantSource {
            type Value = RtdValue;
            fn subscribe(
                &self,
                _: &RtdTopic,
                _: RtdSink<Self::Value>,
            ) -> XllResult<Box<dyn RtdSubscription>> {
                Ok(Box::new(TestSubscription {
                    canceled: Arc::new(AtomicBool::new(false)),
                    disconnected: Arc::new(AtomicBool::new(false)),
                }))
            }
        }

        let source = ReentrantSource {
            runtime: Arc::clone(&runtime),
        };
        let prep = runtime
            .prepare(source, RtdTopic::single("reentrant").unwrap())
            .unwrap();
        let key = SubscriptionKey::new(prep.key());
        prep.commit();

        let conn = runtime
            .connect_transaction(&server, TopicId(1), &key)
            .unwrap();
        conn.commit().unwrap();

        runtime.close().unwrap();
    }

    #[test]
    fn server_lifecycle_rejects_mutations_when_closing() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let server = runtime.register_server(ServerGeneration(1)).unwrap();

        {
            let mut state = server.inner.state.lock();
            state.lifecycle = ServerLifecycle::Closing;
        }

        assert!(matches!(
            server.attach_update_callback(Arc::new(|| Ok(()))),
            Err(XllError::Closing)
        ));
        assert!(matches!(
            server.pulse_notification(),
            Err(XllError::Closing)
        ));
        assert!(matches!(server.begin_refresh(), Err(XllError::Closing)));
        assert!(matches!(
            server.claim(&SubscriptionKey::new("test")),
            Err(XllError::Closing)
        ));
        assert!(matches!(
            server.disconnect(TopicId(1)),
            Err(XllError::Closing)
        ));
    }
}
