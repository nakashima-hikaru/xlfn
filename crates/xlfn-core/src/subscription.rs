#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::{ExcelErrorValue, XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
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

    fn into_excel(self, _: &mut crate::ReturnContext<'_, '_>) -> XllResult<Self::Output> {
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
        if self.created {
            let runtime = self.runtime.upgrade().ok_or(XllError::Closing)?;
            runtime.commit_connection(
                self.owner,
                self.generation,
                &self.key,
                self.observed_sequence,
            )?;
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

#[must_use]
pub(crate) struct RtdUpdateBatch {
    pub(crate) server_generation: u64,
    pub(crate) refresh_id: u64,
    pub(crate) updates: Vec<RtdUpdate>,
}

type NotificationCallback = Arc<dyn Fn() -> XllResult<()> + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalState {
    Dormant,
    Calling { ticket: u64, attempt: u8 },
    Signaled { ticket: u64 },
}

#[derive(Debug)]
enum DeliveryPhase {
    BetweenRefreshes {
        signal: SignalState,
    },
    Refreshing {
        refresh_id: u64,
        #[allow(dead_code)]
        snapshot_max_sequence: u64,
        #[allow(dead_code)]
        consumed_signal: SignalState,
        next_signal: SignalState,
    },
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
    updates: BTreeMap<i32, QueuedUpdate>,
    next_update_sequence: u64,
    next_notification_ticket: u64,
    next_refresh_id: u64,
    phase: DeliveryPhase,
}

#[derive(Clone)]
struct NotificationAttempt {
    server_generation: u64,
    ticket: u64,
    attempt: u8,
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

    fn arm_notification_if_needed(
        &mut self,
        server_generation: u64,
    ) -> XllResult<Option<NotificationAttempt>> {
        if self.updates.is_empty() {
            return Ok(None);
        }

        let Some(callback) = self.callback.as_ref().cloned() else {
            return Ok(None);
        };

        let ticket = self.next_notification_ticket;
        self.next_notification_ticket = ticket.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5449_434b_4f56_464c,
        })?;

        let attempt = match &mut self.phase {
            DeliveryPhase::BetweenRefreshes { signal } => {
                if !matches!(signal, SignalState::Dormant) {
                    return Ok(None);
                }
                *signal = SignalState::Calling { ticket, attempt: 0 };
                Some(NotificationAttempt {
                    server_generation,
                    ticket,
                    attempt: 0,
                    callback,
                })
            }
            DeliveryPhase::Refreshing { next_signal, .. } => {
                if !matches!(next_signal, SignalState::Dormant) {
                    return Ok(None);
                }
                *next_signal = SignalState::Calling { ticket, attempt: 0 };
                None
            }
        };

        Ok(attempt)
    }

    fn signal_for_ticket_mut(&mut self, ticket: u64) -> Option<&mut SignalState> {
        let signal = match &mut self.phase {
            DeliveryPhase::BetweenRefreshes { signal } => signal,
            DeliveryPhase::Refreshing { next_signal, .. } => next_signal,
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
    pending_topic_bytes: usize,
    active: HashMap<TopicOwner, ActiveSubscription>,
    topic_ids: HashMap<String, TopicOwner>,
    deliveries: HashMap<u64, ServerDeliveryState>,
    queued_update_count: usize,
    source_ids: HashMap<usize, SourceIdentity>,
}

fn restore_source_identity(
    state: &mut SubscriptionState,
    ptr_key: usize,
    inserted: bool,
    previous: Option<SourceIdentity>,
) {
    if !inserted {
        return;
    }
    if let Some(previous) = previous {
        state.source_ids.insert(ptr_key, previous);
    } else {
        state.source_ids.remove(&ptr_key);
    }
}

pub(crate) struct SubscriptionRuntime {
    limits: RtdLimits,
    module_ingress: Option<&'static crate::ingress::ExportIngress>,
    state: Mutex<SubscriptionState>,
    idle: Condvar,
    cleanup_failure: Mutex<Option<XllError>>,
    next_preparation_id: AtomicU64,
    next_generation: AtomicU64,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

pub(crate) struct SubscriptionOperation<'a> {
    runtime: &'a SubscriptionRuntime,
    server_generation: Option<u64>,
    _ingress_guard: Option<crate::ingress::ExportCallGuard<'static>>,
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
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.runtime
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
    }
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
            state: Mutex::new(SubscriptionState {
                closed: false,
                in_flight: 0,
                in_flight_by_server: HashMap::new(),
                terminating_servers: HashSet::new(),
                terminated_servers: HashSet::new(),
                pending: HashMap::new(),
                pending_topic_bytes: 0,
                active: HashMap::new(),
                topic_ids: HashMap::new(),
                deliveries: HashMap::new(),
                queued_update_count: 0,
                source_ids: HashMap::new(),
            }),
            idle: Condvar::new(),
            cleanup_failure: Mutex::new(None),
            next_preparation_id: AtomicU64::new(1),
            next_generation: AtomicU64::new(1),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
        }
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
        if let Some(ingress) = self.module_ingress {
            let mut admitted = false;
            let mut admission_error = None;
            let (ingress_guard, accepted) = ingress.enter_with(|| {
                let mut state = self.state.lock();
                match self.admit_operation_locked(&mut state, server_generation) {
                    Ok(()) => {
                        admitted = true;
                        #[cfg(any(test, feature = "shutdown-refinement"))]
                        self.record_ghost_event(
                            crate::shutdown_refinement::GhostEvent::BeginRtdOperation,
                        );
                    }
                    Err(error) => admission_error = Some(error),
                }
            });
            if !accepted {
                return Err(XllError::Closing);
            }
            if !admitted {
                drop(ingress_guard);
                return Err(admission_error.expect("RTD admission error is recorded"));
            }
            return Ok(SubscriptionOperation {
                runtime: self,
                server_generation,
                _ingress_guard: Some(ingress_guard),
            });
        }

        let mut state = self.state.lock();
        let result = self.enter_operation_locked(&mut state, server_generation);
        drop(state);
        if result.is_ok() {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
        }
        result
    }

    fn enter_operation_locked<'a>(
        &'a self,
        state: &mut SubscriptionState,
        server_generation: Option<u64>,
    ) -> XllResult<SubscriptionOperation<'a>> {
        self.admit_operation_locked(state, server_generation)?;
        Ok(SubscriptionOperation {
            runtime: self,
            server_generation,
            _ingress_guard: None,
        })
    }

    fn admit_operation_locked(
        &self,
        state: &mut SubscriptionState,
        server_generation: Option<u64>,
    ) -> XllResult<()> {
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
        Ok(())
    }

    pub(crate) fn enter_server_operation(
        &self,
        server_generation: u64,
    ) -> XllResult<SubscriptionOperation<'_>> {
        self.enter_operation(Some(server_generation))
    }

    pub(crate) fn enter_external_operation(&self) -> XllResult<SubscriptionOperation<'_>> {
        self.enter_operation(None)
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
        topic.validate_with_limits(&self.limits)?;
        let topic_bytes = topic.byte_len();
        let ptr_key = Arc::as_ptr(&source) as usize;
        let erased_source: Arc<dyn Any + Send + Sync> = source.clone();
        let mut state = self.state.lock();
        if state.closed {
            return Err(XllError::Closing);
        }
        state
            .source_ids
            .retain(|_, identity| identity.source.strong_count() != 0);
        let mut replaced_source_identity = None;
        let mut source_identity_inserted = false;
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
                if state.source_ids.len() >= self.limits.max_source_ids {
                    return Err(XllError::Overloaded);
                }
                let id = NEXT_SOURCE_ID.fetch_add(1, Ordering::Relaxed);
                source_identity_inserted = true;
                replaced_source_identity = state.source_ids.insert(
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

        let Some(next_pending_topic_bytes) = state.pending_topic_bytes.checked_add(topic_bytes)
        else {
            restore_source_identity(
                &mut state,
                ptr_key,
                source_identity_inserted,
                replaced_source_identity,
            );
            return Err(XllError::Overloaded);
        };
        if state.pending.len() >= self.limits.max_pending
            || next_pending_topic_bytes > self.limits.max_total_topic_bytes
        {
            restore_source_identity(
                &mut state,
                ptr_key,
                source_identity_inserted,
                replaced_source_identity,
            );
            return Err(XllError::Overloaded);
        }

        let preparation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);
        state.pending_topic_bytes = next_pending_topic_bytes;
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
                        observed_sequence: None,
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
            if state.active.len() >= self.limits.max_active {
                return Err(XllError::Overloaded);
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
            let installed = state
                .active
                .get(&owner)
                .is_some_and(|active| can_install && active.generation == generation);
            let installed = if installed {
                state
                    .active
                    .get_mut(&owner)
                    .expect("the active RTD connection was checked above")
                    .subscription = subscription.take();
                let active = state
                    .active
                    .get(&owner)
                    .expect("the active RTD connection was installed above");
                Some((
                    active.latest.clone(),
                    state
                        .deliveries
                        .get(&owner.server_generation)
                        .and_then(|d| d.updates.get(&owner.topic_id))
                        .map(|queued| queued.sequence),
                ))
            } else {
                None
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
            Some((value, observed_sequence)) => Ok(SubscriptionConnection {
                runtime: Arc::downgrade(self),
                owner,
                generation,
                key: key.to_owned(),
                value,
                observed_sequence,
                created: true,
                finished: false,
            }),
            None => Err(XllError::Closing),
        }
    }

    fn commit_connection(
        &self,
        owner: TopicOwner,
        generation: u64,
        key: &str,
        observed_sequence: Option<u64>,
    ) -> XllResult<()> {
        let _operation = self.enter_operation(Some(owner.server_generation))?;
        let (retired_pending, attempt) = {
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
            let delivery = state.deliveries.entry(owner.server_generation).or_default();
            let mut removed_update = false;
            let should_keep = match delivery.updates.get(&owner.topic_id) {
                Some(queued) if observed_sequence == Some(queued.sequence) => {
                    delivery.updates.remove(&owner.topic_id);
                    removed_update = true;
                    false
                }
                Some(_) => true,
                None => false,
            };
            let attempt = if should_keep {
                delivery.arm_notification_if_needed(owner.server_generation)?
            } else {
                None
            };
            if removed_update {
                state.queued_update_count = state.queued_update_count.saturating_sub(1);
            }
            (Self::remove_pending(&mut state, key), attempt)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            retired_pending,
            "rtd_committed_pending_source_drop",
        ));
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddSubscription);
        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
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
                if state
                    .deliveries
                    .get_mut(&owner.server_generation)
                    .is_some_and(|delivery| delivery.updates.remove(&owner.topic_id).is_some())
                {
                    state.queued_update_count = state.queued_update_count.saturating_sub(1);
                }
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
                .then(|| Self::remove_pending(&mut state, key))
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
            if state
                .deliveries
                .get_mut(&owner.server_generation)
                .is_some_and(|delivery| delivery.updates.remove(&owner.topic_id).is_some())
            {
                state.queued_update_count = state.queued_update_count.saturating_sub(1);
            }
        }
    }

    fn remove_pending(state: &mut SubscriptionState, key: &str) -> Option<PendingSubscription> {
        let pending = state.pending.remove(key)?;
        state.pending_topic_bytes = state
            .pending_topic_bytes
            .checked_sub(pending.topic.byte_len())
            .expect("pending RTD topic byte quota remains balanced");
        Some(pending)
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
        should_remove
            .then(|| Self::remove_pending(state, key))
            .flatten()
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
            if state
                .deliveries
                .get_mut(&server_generation)
                .is_some_and(|delivery| delivery.updates.remove(&topic_id).is_some())
            {
                state.queued_update_count = state.queued_update_count.saturating_sub(1);
            }
            active
                .subscription
                .map(|subscription| (active.key, subscription))
        };
        if let Some((key, subscription)) = subscription {
            self.record_cleanup_result(disconnect_one_no_unwind(subscription, owner, &key));
        }
    }

    pub(crate) fn begin_refresh(&self, server_generation: u64) -> XllResult<RtdUpdateBatch> {
        let mut state = self.state.lock();

        if state.closed
            || state.terminating_servers.contains(&server_generation)
            || state.terminated_servers.contains(&server_generation)
        {
            return Err(XllError::Closing);
        }

        let committed_topics: std::collections::HashSet<i32> = state
            .active
            .iter()
            .filter(|(owner, active)| {
                owner.server_generation == server_generation && active.committed
            })
            .map(|(owner, _)| owner.topic_id)
            .collect();

        let delivery = state
            .deliveries
            .get_mut(&server_generation)
            .ok_or(XllError::Closing)?;

        let refresh_id = delivery.allocate_refresh_id()?;

        let updates: Vec<RtdUpdate> = delivery
            .updates
            .iter()
            .filter(|(topic_id, _)| committed_topics.contains(topic_id))
            .map(|(&topic_id, queued)| RtdUpdate {
                sequence: queued.sequence,
                topic_id,
                value: queued.value.clone(),
            })
            .collect();

        let snapshot_max_sequence = updates.iter().map(|u| u.sequence).max().unwrap_or(0);

        let previous_phase = std::mem::replace(
            &mut delivery.phase,
            DeliveryPhase::BetweenRefreshes {
                signal: SignalState::Dormant,
            },
        );

        let consumed_signal = match previous_phase {
            DeliveryPhase::BetweenRefreshes { signal } => signal,
            DeliveryPhase::Refreshing { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: 0x4f56_4c50_5245_4652,
                });
            }
        };

        delivery.phase = DeliveryPhase::Refreshing {
            refresh_id,
            snapshot_max_sequence,
            consumed_signal,
            next_signal: SignalState::Dormant,
        };

        Ok(RtdUpdateBatch {
            server_generation,
            refresh_id,
            updates,
        })
    }

    pub(crate) fn complete_refresh(&self, batch: RtdUpdateBatch, outcome: RefreshOutcome) {
        let attempt = {
            let mut state = self.state.lock();

            let Some(delivery) = state.deliveries.get_mut(&batch.server_generation) else {
                return;
            };

            let DeliveryPhase::Refreshing { refresh_id, .. } = &mut delivery.phase else {
                return;
            };

            if *refresh_id != batch.refresh_id {
                return;
            }

            let mut removed_count = 0_usize;
            if outcome == RefreshOutcome::Delivered {
                for update in &batch.updates {
                    if delivery
                        .updates
                        .get(&update.topic_id)
                        .is_some_and(|queued| queued.sequence == update.sequence)
                    {
                        delivery.updates.remove(&update.topic_id);
                        removed_count += 1;
                    }
                }
            }

            delivery.phase = DeliveryPhase::BetweenRefreshes {
                signal: SignalState::Dormant,
            };

            let attempt = if !delivery.updates.is_empty() {
                delivery
                    .arm_notification_if_needed(batch.server_generation)
                    .ok()
                    .flatten()
            } else {
                None
            };

            state.queued_update_count = state.queued_update_count.saturating_sub(removed_count);

            attempt
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
    }

    pub(crate) fn attach_update_callback(
        &self,
        server_generation: u64,
        callback: NotificationCallback,
    ) -> XllResult<()> {
        let _operation = self.enter_operation(Some(server_generation))?;
        let (retired, attempt) = {
            let mut state = self.state.lock();

            if state.closed
                || state.terminating_servers.contains(&server_generation)
                || state.terminated_servers.contains(&server_generation)
            {
                return Err(XllError::Closing);
            }

            let delivery = state.deliveries.entry(server_generation).or_default();

            let retired = delivery.callback.replace(callback);
            let attempt = delivery.arm_notification_if_needed(server_generation)?;
            (retired, attempt)
        };

        drop(retired);

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }

        Ok(())
    }

    pub(crate) fn detach_update_callback(&self, server_generation: u64) {
        let retired = {
            let mut state = self.state.lock();
            if let Some(delivery) = state.deliveries.get_mut(&server_generation) {
                delivery.callback.take()
            } else {
                None
            }
        };
        drop(retired);
    }

    pub(crate) fn pulse_notification(&self, server_generation: u64) {
        let Ok(_operation) = self.enter_operation(Some(server_generation)) else {
            return;
        };
        let attempt = {
            let mut state = self.state.lock();
            state.deliveries.get_mut(&server_generation).and_then(|d| {
                d.arm_notification_if_needed(server_generation)
                    .ok()
                    .flatten()
            })
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self, server_generation: u64) -> usize {
        let state = self.state.lock();
        let state_ref = &*state;
        if let Some(delivery) = state_ref.deliveries.get(&server_generation) {
            delivery
                .updates
                .iter()
                .filter(|(topic_id, _)| {
                    state_ref
                        .active
                        .get(&TopicOwner {
                            server_generation,
                            topic_id: **topic_id,
                        })
                        .is_some_and(|active| active.committed)
                })
                .count()
        } else {
            0
        }
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
                return Err(XllError::Closing);
            }
            state.terminating_servers.insert(server_generation);
        }

        let retired_delivery = {
            let mut state = self.state.lock();
            state.deliveries.remove(&server_generation)
        };
        if let Some(delivery) = retired_delivery {
            let removed_count = delivery.updates.len();
            let mut state = self.state.lock();
            state.queued_update_count = state.queued_update_count.saturating_sub(removed_count);
            drop(state);
            drop(delivery.callback);
        }

        let (mut subscriptions, _removed_subscriptions) = {
            let mut state = self.state.lock();
            let subscriptions = state
                .active
                .iter_mut()
                .filter_map(|(owner, active)| {
                    if owner.server_generation != server_generation {
                        return None;
                    }
                    let committed = active.committed;
                    active
                        .subscription
                        .take()
                        .map(|subscription| (*owner, active.key.clone(), committed, subscription))
                })
                .collect::<Vec<_>>();
            let removed = subscriptions
                .iter()
                .filter(|(_, _, committed, _)| *committed)
                .count();
            let subscriptions = subscriptions
                .into_iter()
                .map(|(owner, key, _, subscription)| (owner, key, subscription))
                .collect::<Vec<_>>();
            (subscriptions, removed)
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

        let (late_subscriptions, removed_pending, _late_removed_subscriptions) = {
            let mut state = self.state.lock();
            let pending_keys = state
                .pending
                .iter()
                .filter(|(_, pending)| pending.server_generation == Some(server_generation))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            let removed_pending = pending_keys
                .into_iter()
                .filter_map(|key| Self::remove_pending(&mut state, &key))
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
                    if state
                        .deliveries
                        .get_mut(&owner.server_generation)
                        .is_some_and(|delivery| delivery.updates.remove(&owner.topic_id).is_some())
                    {
                        state.queued_update_count = state.queued_update_count.saturating_sub(1);
                    }
                    let active = state.active.remove(&owner)?;
                    state.topic_ids.remove(&active.key);
                    active
                        .subscription
                        .map(|subscription| (owner, active.key, active.committed, subscription))
                })
                .collect::<Vec<_>>();
            let late_removed = late_subscriptions
                .iter()
                .filter(|(_, _, committed, _)| *committed)
                .count();
            let late_subscriptions = late_subscriptions
                .into_iter()
                .map(|(owner, key, _, subscription)| (owner, key, subscription))
                .collect::<Vec<_>>();
            (late_subscriptions, removed_pending, late_removed)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            removed_pending,
            "rtd_termination_pending_source_drop",
        ));
        self.record_cleanup_result(request_cancel_all_no_unwind(&late_subscriptions));
        subscriptions.extend(late_subscriptions);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..(_removed_subscriptions + _late_removed_subscriptions) {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveSubscription);
        }
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
        let (mut subscriptions, _removed_subscriptions) = {
            let mut state = self.state.lock();
            state.closed = true;
            let subscriptions = state
                .active
                .iter_mut()
                .filter_map(|(owner, active)| {
                    let committed = active.committed;
                    active
                        .subscription
                        .take()
                        .map(|subscription| (*owner, active.key.clone(), committed, subscription))
                })
                .collect::<Vec<_>>();
            let removed = subscriptions
                .iter()
                .filter(|(_, _, committed, _)| *committed)
                .count();
            let subscriptions = subscriptions
                .into_iter()
                .map(|(owner, key, _, subscription)| (owner, key, subscription))
                .collect::<Vec<_>>();
            (subscriptions, removed)
        };
        let retired_deliveries = {
            let mut state = self.state.lock();
            let deliveries = std::mem::take(&mut state.deliveries);
            state.queued_update_count = 0;
            deliveries
        };
        drop(retired_deliveries);
        self.record_cleanup_result(request_cancel_all_no_unwind(&subscriptions));

        let (late_subscriptions, removed_pending, _late_removed_subscriptions) = {
            let mut state = self.state.lock();
            while state.in_flight != 0 {
                self.idle.wait(&mut state);
            }
            let removed_pending = std::mem::take(&mut state.pending);
            state.pending_topic_bytes = 0;
            state.topic_ids.clear();
            state.deliveries.clear();
            state.queued_update_count = 0;
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
                        .map(|subscription| (owner, active.key, active.committed, subscription))
                })
                .collect::<Vec<_>>();
            let late_removed = late_subscriptions
                .iter()
                .filter(|(_, _, committed, _)| *committed)
                .count();
            let late_subscriptions = late_subscriptions
                .into_iter()
                .map(|(owner, key, _, subscription)| (owner, key, subscription))
                .collect::<Vec<_>>();
            (late_subscriptions, removed_pending, late_removed)
        };
        self.record_cleanup_result(drop_pending_subscriptions_no_unwind(
            removed_pending.into_values(),
            "rtd_close_pending_source_drop",
        ));
        self.record_cleanup_result(request_cancel_all_no_unwind(&late_subscriptions));
        subscriptions.extend(late_subscriptions);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..(_removed_subscriptions + _late_removed_subscriptions) {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveSubscription);
        }
        self.record_cleanup_result(disconnect_all_no_unwind(subscriptions));

        self.cleanup_result()
    }

    fn publish(&self, owner: TopicOwner, generation: u64, value: RtdValue) -> XllResult<()> {
        let _operation = self.enter_operation(Some(owner.server_generation))?;
        let attempt = {
            let mut state = self.state.lock();
            if state.closed
                || state.terminating_servers.contains(&owner.server_generation)
                || state.terminated_servers.contains(&owner.server_generation)
            {
                return Err(XllError::Closing);
            }
            let active = state
                .active
                .get_mut(&owner)
                .filter(|active| active.generation == generation)
                .ok_or(XllError::Closing)?;
            active.latest = value.clone();
            let is_committed = active.committed;

            let is_new_topic = !state
                .deliveries
                .get(&owner.server_generation)
                .is_some_and(|d| d.updates.contains_key(&owner.topic_id));

            if is_new_topic && state.queued_update_count >= self.limits.max_queued_updates {
                return Err(XllError::Overloaded);
            }

            let delivery = state.deliveries.entry(owner.server_generation).or_default();

            let sequence = delivery.allocate_update_sequence()?;
            delivery
                .updates
                .insert(owner.topic_id, QueuedUpdate { sequence, value });
            let attempt = if is_committed {
                delivery.arm_notification_if_needed(owner.server_generation)?
            } else {
                None
            };

            if is_new_topic {
                state.queued_update_count += 1;
            }

            attempt
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
        Ok(())
    }

    fn drive_notification(&self, mut attempt: NotificationAttempt) {
        const MAX_ATTEMPTS: u8 = 3;
        loop {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginCallback);

            let callback = Arc::clone(&attempt.callback);
            let result = match catch_unwind(AssertUnwindSafe(|| callback())) {
                Ok(res) => res,
                Err(_) => Err(XllError::Panic),
            };

            #[cfg(any(test, feature = "shutdown-refinement"))]
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndCallback);

            match self.finish_notification_attempt(&attempt, result, MAX_ATTEMPTS) {
                NotificationCompletion::Finished => return,
                NotificationCompletion::Retry(next) => {
                    std::thread::yield_now();
                    attempt = next;
                }
                NotificationCompletion::Failed(error) => {
                    crate::diagnostics::report_no_unwind("rtd_update_notify", &error);
                    return;
                }
            }
        }
    }

    fn finish_notification_attempt(
        &self,
        attempt: &NotificationAttempt,
        result: XllResult<()>,
        max_attempts: u8,
    ) -> NotificationCompletion {
        let mut state = self.state.lock();

        let Some(delivery) = state.deliveries.get_mut(&attempt.server_generation) else {
            return NotificationCompletion::Finished;
        };

        let callback = delivery.callback.clone();
        let Some(signal) = delivery.signal_for_ticket_mut(attempt.ticket) else {
            return NotificationCompletion::Finished;
        };

        match result {
            Ok(()) => {
                *signal = SignalState::Signaled {
                    ticket: attempt.ticket,
                };
                NotificationCompletion::Finished
            }
            Err(_error) if attempt.attempt + 1 < max_attempts => {
                let next_attempt = attempt.attempt + 1;
                *signal = SignalState::Calling {
                    ticket: attempt.ticket,
                    attempt: next_attempt,
                };
                let Some(callback) = callback else {
                    *signal = SignalState::Dormant;
                    return NotificationCompletion::Finished;
                };
                NotificationCompletion::Retry(NotificationAttempt {
                    server_generation: attempt.server_generation,
                    ticket: attempt.ticket,
                    attempt: next_attempt,
                    callback,
                })
            }
            Err(error) => {
                *signal = SignalState::Dormant;
                NotificationCompletion::Failed(error)
            }
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

    type PublishingSink = Arc<Mutex<Option<RtdSink<f64>>>>;

    struct PublishingSource {
        sink: PublishingSink,
        initial: Option<f64>,
        disconnected: Arc<AtomicBool>,
    }

    impl RtdSource for PublishingSource {
        type Value = f64;

        fn subscribe(
            &self,
            _topic: &RtdTopic,
            sink: RtdSink<Self::Value>,
        ) -> XllResult<Box<dyn RtdSubscription>> {
            if let Some(initial) = self.initial {
                sink.publish(initial)?;
            }
            self.sink.lock().replace(sink);
            Ok(Box::new(TestSubscription(Arc::clone(&self.disconnected))))
        }
    }

    fn publishing_source(
        initial: Option<f64>,
    ) -> (Arc<PublishingSource>, PublishingSink, Arc<AtomicBool>) {
        let sink = Arc::new(Mutex::new(None));
        let disconnected = Arc::new(AtomicBool::new(false));
        let source = Arc::new(PublishingSource {
            sink: Arc::clone(&sink),
            initial,
            disconnected: Arc::clone(&disconnected),
        });
        (source, sink, disconnected)
    }

    #[test]
    fn synchronous_initial_publish_is_isolated_until_connection_commit() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(1, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        let (source, _sink, _disconnected) = publishing_source(Some(12.5));
        let prepared = runtime
            .prepare(source, RtdTopic::single("initial-isolation").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();

        let connection = runtime.connect_transaction(1, 1, &key).unwrap();
        assert_eq!(connection.value(), &RtdValue::Number(12.5));
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.pending_update_count(1), 0);

        connection.commit().unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.pending_update_count(1), 0);
    }

    #[test]
    fn snapshot_updates_excludes_an_uncommitted_connection() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (source, _sink, _disconnected) = publishing_source(Some(12.5));
        let prepared = runtime
            .prepare(source, RtdTopic::single("snapshot-isolation").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();

        let connection = runtime.connect_transaction(2, 2, &key).unwrap();
        assert_eq!(runtime.pending_update_count(2), 0);
        drop(connection);
        assert_eq!(runtime.pending_update_count(2), 0);
    }

    #[test]
    fn failed_initial_value_write_leaves_no_notification_history() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(3, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        let (source, _sink, _disconnected) = publishing_source(Some(12.5));
        let prepared = runtime
            .prepare(source, RtdTopic::single("failed-write").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();

        let connection = runtime.connect_transaction(3, 3, &key).unwrap();
        assert_eq!(connection.value(), &RtdValue::Number(12.5));
        // Model ConnectData's failed VARIANT write: dropping the uncommitted
        // connection must roll back the source and its queued initial value.
        drop(connection);

        assert_eq!(notifications.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.pending_update_count(3), 0);
        assert!(runtime.state.lock().active.is_empty());
    }

    #[test]
    fn publish_before_commit_notifies_once_after_commit() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(4, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        let (source, sink, _disconnected) = publishing_source(Some(12.5));
        let prepared = runtime
            .prepare(source, RtdTopic::single("publish-before-commit").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();

        let connection = runtime.connect_transaction(4, 4, &key).unwrap();
        sink.lock()
            .as_ref()
            .expect("source captured the RTD sink")
            .publish(13.5)
            .unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.pending_update_count(4), 0);

        connection.commit().unwrap();

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        let batch = runtime.begin_refresh(4).unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, RtdValue::Number(13.5));
        runtime.complete_refresh(batch, RefreshOutcome::Delivered);
    }

    #[test]
    fn rtd_topic_limits_are_checked_before_subscription_admission() {
        let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
            max_topic_parts: 1,
            max_topic_bytes: 3,
            ..RtdLimits::standard()
        }));
        let source = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });

        let too_many_parts = RtdTopic::new(["one", "two"]).unwrap();
        assert!(matches!(
            runtime.prepare(Arc::clone(&source), too_many_parts),
            Err(XllError::Input {
                reason: crate::InputError::TooLarge { .. },
                ..
            })
        ));

        let too_many_bytes = RtdTopic::single("four").unwrap();
        assert!(matches!(
            runtime.prepare(source, too_many_bytes),
            Err(XllError::Input {
                reason: crate::InputError::TooLarge { .. },
                ..
            })
        ));
    }

    #[test]
    fn pending_and_active_rtd_quotas_are_released_by_transaction_cleanup() {
        let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
            max_pending: 1,
            max_active: 1,
            ..RtdLimits::standard()
        }));
        let first = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let second = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });

        let pending = runtime
            .prepare(Arc::clone(&first), RtdTopic::single("pending").unwrap())
            .unwrap();
        assert!(matches!(
            runtime.prepare(Arc::clone(&first), RtdTopic::single("same-source").unwrap()),
            Err(XllError::Overloaded)
        ));
        assert!(matches!(
            runtime.prepare(Arc::clone(&second), RtdTopic::single("blocked").unwrap()),
            Err(XllError::Overloaded)
        ));
        assert_eq!(runtime.state.lock().source_ids.len(), 1);
        drop(pending);
        let active_key = runtime
            .prepare(Arc::clone(&first), RtdTopic::single("active").unwrap())
            .unwrap();
        let active_key = active_key.key().to_owned();
        runtime
            .connect_transaction(1, 1, &active_key)
            .unwrap()
            .commit()
            .unwrap();

        let blocked_key = runtime
            .prepare(second, RtdTopic::single("blocked").unwrap())
            .unwrap();
        let blocked_key = blocked_key.key().to_owned();
        let preparation = match runtime.connect_transaction(1, 2, &blocked_key) {
            Ok(_) => panic!("active RTD quota unexpectedly admitted a second stream"),
            Err(error) => error,
        };
        assert!(matches!(preparation, XllError::Overloaded));

        runtime.disconnect(1, 1);
        runtime
            .connect_transaction(1, 2, &blocked_key)
            .unwrap()
            .commit()
            .unwrap();
    }

    #[test]
    fn aggregate_pending_topic_bytes_are_released_with_the_pending_entry() {
        let runtime = Arc::new(SubscriptionRuntime::with_limits(RtdLimits {
            max_total_topic_bytes: 3,
            ..RtdLimits::standard()
        }));
        let first = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let second = Arc::new(TestSource {
            disconnected: Arc::new(AtomicBool::new(false)),
        });
        let pending = runtime
            .prepare(Arc::clone(&first), RtdTopic::single("one").unwrap())
            .unwrap();
        assert!(matches!(
            runtime.prepare(Arc::clone(&second), RtdTopic::single("two").unwrap()),
            Err(XllError::Overloaded)
        ));
        drop(pending);
        let released = runtime
            .prepare(second, RtdTopic::single("two").unwrap())
            .unwrap();
        released.rollback();
    }

    struct ReentrantDropSource {
        runtime: Weak<SubscriptionRuntime>,
        dropped: mpsc::SyncSender<()>,
    }

    impl Drop for ReentrantDropSource {
        fn drop(&mut self) {
            if let Some(runtime) = self.runtime.upgrade() {
                runtime.detach_update_callback(9_999);
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
        assert_eq!(runtime.pending_update_count(1), 0);
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
        assert_eq!(state.queued_update_count, 0);
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
        assert_eq!(state.queued_update_count, 0);
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
        assert!(state.deliveries.is_empty());
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
        let first = runtime.begin_refresh(1).unwrap();
        sink.publish(2.0).unwrap();

        runtime.complete_refresh(first, RefreshOutcome::Delivered);

        let remaining = runtime.begin_refresh(1).unwrap();
        assert_eq!(remaining.updates.len(), 1);
        assert_eq!(remaining.updates[0].value, RtdValue::Number(2.0));
        runtime.complete_refresh(remaining, RefreshOutcome::Delivered);
    }

    #[test]
    fn update_notify_failure_is_retried_and_reported() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let succeed = Arc::new(AtomicBool::new(false));

        runtime
            .attach_update_callback(1, {
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
            })
            .unwrap();

        let (source, sink, _disconnected) = publishing_source(None);
        let key = runtime
            .prepare(source, RtdTopic::single("notify-fail").unwrap())
            .unwrap();
        runtime.connect(1, 15, key.key()).unwrap();
        sink.lock()
            .as_ref()
            .expect("source captured the RTD sink")
            .publish(12.5)
            .unwrap();

        // A committed publication attempts bounded retries (3 times) and fails.
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        // Verify pending updates remain in the queue
        assert_eq!(runtime.pending_update_count(1), 1);

        // Enable success and trigger retry via heartbeat / pulse_notification
        succeed.store(true, Ordering::SeqCst);
        runtime.pulse_notification(1);

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
                    runtime.detach_update_callback(self.server_generation);
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
        runtime
            .attach_update_callback(
                41,
                Arc::new(move || {
                    let _keep_drop_live = &reentrant;
                    Ok(())
                }),
            )
            .unwrap();

        runtime
            .attach_update_callback(41, Arc::new(|| Ok(())))
            .unwrap();

        dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
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
                    runtime.detach_update_callback(42);
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
        runtime
            .attach_update_callback(
                42,
                Arc::new(move || {
                    let _keep_drop_live = &reentrant;
                    Ok(())
                }),
            )
            .unwrap();

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
                    runtime.detach_update_callback(43);
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
        runtime
            .attach_update_callback(
                43,
                Arc::new(move || {
                    let _keep_drop_live = &reentrant;
                    Ok(())
                }),
            )
            .unwrap();

        drop(runtime.terminate_server(43).unwrap());

        dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(runtime.state.lock().terminated_servers.contains(&43));
    }

    #[test]
    fn failed_refresh_can_renotify_without_consuming_the_batch() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(1, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        let (source, sink, _disconnected) = publishing_source(None);
        let key = runtime
            .prepare(source, RtdTopic::single("retry-refresh").unwrap())
            .unwrap();
        runtime.connect(1, 12, key.key()).unwrap();
        sink.lock()
            .as_ref()
            .expect("source captured the RTD sink")
            .publish(12.5)
            .unwrap();
        let batch = runtime.begin_refresh(1).unwrap();

        runtime.complete_refresh(batch, RefreshOutcome::Failed);

        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.pending_update_count(1), 1);
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
        runtime
            .attach_update_callback(
                2,
                Arc::new(move || {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap();
                    Ok(())
                }),
            )
            .unwrap();

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
        runtime
            .attach_update_callback(
                3,
                Arc::new(move || {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap();
                    Ok(())
                }),
            )
            .unwrap();

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

    #[test]
    fn same_topic_burst_coalesces_to_single_notification() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(100, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("burst-same").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(100, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        for i in 0..1000 {
            sink.publish(i as f64).unwrap();
        }

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.pending_update_count(100), 1);

        let batch = runtime.begin_refresh(100).unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, RtdValue::Number(999.0));
        runtime.complete_refresh(batch, RefreshOutcome::Delivered);
        assert_eq!(runtime.pending_update_count(100), 0);
    }

    #[test]
    fn distinct_topics_burst_coalesces_to_single_notification() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(101, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let mut sinks = Vec::new();
        for i in 0..10i32 {
            let (source, sink, _) = publishing_source(None);
            let prepared = runtime
                .prepare(source, RtdTopic::single(format!("topic-{}", i)).unwrap())
                .unwrap();
            let key = prepared.key().to_owned();
            prepared.commit();
            runtime.connect(101, i, &key).unwrap();
            sinks.push(sink.lock().clone().unwrap());
        }

        for sink in &sinks {
            for v in 0..100 {
                sink.publish(v as f64).unwrap();
            }
        }

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.pending_update_count(101), 10);

        let batch = runtime.begin_refresh(101).unwrap();
        assert_eq!(batch.updates.len(), 10);
        runtime.complete_refresh(batch, RefreshOutcome::Delivered);
        assert_eq!(runtime.pending_update_count(101), 0);
    }

    #[test]
    fn publish_during_notify_coalesces_next_signal() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));

        let (source, sink_slot, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("reentrant-pub").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(102, 1, &key).unwrap();

        let sink = sink_slot.lock().clone().unwrap();

        let sink_clone = sink.clone();
        let notifications_clone = Arc::clone(&notifications);
        runtime
            .attach_update_callback(
                102,
                Arc::new(move || {
                    let count = notifications_clone.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        sink_clone.publish(200.0).unwrap();
                    }
                    Ok(())
                }),
            )
            .unwrap();

        sink.publish(100.0).unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        let batch = runtime.begin_refresh(102).unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, RtdValue::Number(200.0));

        runtime.complete_refresh(batch, RefreshOutcome::Delivered);
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn publish_during_refresh_arms_next_signal() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(103, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("during-refresh").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(103, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        let batch = runtime.begin_refresh(103).unwrap();
        assert_eq!(batch.updates.len(), 1);

        sink.publish(2.0).unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        runtime.complete_refresh(batch, RefreshOutcome::Delivered);
        assert_eq!(notifications.load(Ordering::SeqCst), 2);

        let second_batch = runtime.begin_refresh(103).unwrap();
        assert_eq!(second_batch.updates.len(), 1);
        assert_eq!(second_batch.updates[0].value, RtdValue::Number(2.0));
        runtime.complete_refresh(second_batch, RefreshOutcome::Delivered);
    }

    #[test]
    fn same_topic_overwrite_during_refresh_retains_newer_sequence() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        runtime
            .attach_update_callback(104, Arc::new(|| Ok(())))
            .unwrap();

        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("overwrite-refresh").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(104, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();

        let first_batch = runtime.begin_refresh(104).unwrap();
        assert_eq!(first_batch.updates[0].value, RtdValue::Number(1.0));

        sink.publish(2.0).unwrap();

        runtime.complete_refresh(first_batch, RefreshOutcome::Delivered);

        assert_eq!(runtime.pending_update_count(104), 1);
        let second_batch = runtime.begin_refresh(104).unwrap();
        assert_eq!(second_batch.updates[0].value, RtdValue::Number(2.0));
        runtime.complete_refresh(second_batch, RefreshOutcome::Delivered);
    }

    #[test]
    fn failed_refresh_data_does_not_consume_queue_and_retries() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(105, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("fail-refresh").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(105, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(10.0).unwrap();
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        let batch = runtime.begin_refresh(105).unwrap();
        runtime.complete_refresh(batch, RefreshOutcome::Failed);

        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.pending_update_count(105), 1);

        let retry_batch = runtime.begin_refresh(105).unwrap();
        assert_eq!(retry_batch.updates.len(), 1);
        runtime.complete_refresh(retry_batch, RefreshOutcome::Delivered);
        assert_eq!(runtime.pending_update_count(105), 0);
    }

    #[test]
    fn notify_failure_retries_up_to_max_attempts() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(106, {
                let attempts = Arc::clone(&attempts);
                Arc::new(move || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(XllError::Panic)
                })
            })
            .unwrap();

        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("notify-fail-max").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(106, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(55.0).unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.pending_update_count(106), 1);
    }

    #[test]
    fn publish_without_callback_arms_notification_on_attach() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let (source, sink, _) = publishing_source(None);
        let prepared = runtime
            .prepare(source, RtdTopic::single("no-callback").unwrap())
            .unwrap();
        let key = prepared.key().to_owned();
        prepared.commit();
        runtime.connect(107, 1, &key).unwrap();

        let sink = sink.lock().clone().unwrap();
        sink.publish(1.0).unwrap();
        assert_eq!(runtime.pending_update_count(107), 1);

        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(107, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn benchmark_rtd_burst() {
        let runtime = Arc::new(SubscriptionRuntime::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        runtime
            .attach_update_callback(200, {
                let notifications = Arc::clone(&notifications);
                Arc::new(move || {
                    notifications.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();

        let mut sinks = Vec::new();
        for i in 0..100i32 {
            let (source, sink, _) = publishing_source(None);
            let prepared = runtime
                .prepare(
                    source,
                    RtdTopic::single(format!("bench-topic-{}", i)).unwrap(),
                )
                .unwrap();
            let key = prepared.key().to_owned();
            prepared.commit();
            runtime.connect(200, i, &key).unwrap();
            sinks.push(sink.lock().clone().unwrap());
        }

        let start = std::time::Instant::now();
        const PUBLISH_COUNT: usize = 100_000;
        for i in 0..PUBLISH_COUNT {
            sinks[i % 100].publish(i as f64).unwrap();
        }
        let elapsed = start.elapsed();

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.pending_update_count(200), 100);

        let ops_per_sec = (PUBLISH_COUNT as f64) / elapsed.as_secs_f64();
        println!(
            "RTD Coalesced Burst Publish Throughput: {:.2} ops/sec ({:?})",
            ops_per_sec, elapsed
        );
    }
}
