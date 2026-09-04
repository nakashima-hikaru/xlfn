#![allow(
    unsafe_code,
    reason = "ErasedSink is an audited non-owning capability over a runtime-owned PublishCore"
)]

use super::data_plane::PublishCore;
use super::host::SubscriptionHost;
use super::topic::{SubscriptionId, TopicId};
#[cfg(test)]
use super::value::RtdValue;
use super::value::StoredRtdValue;
use crate::generation::ConnectionGeneration;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::ptr::NonNull;
use xlfn_kernel::quota::QuotaPermit;

#[derive(Clone, Copy)]
pub(crate) struct ErasedSink {
    publish_core: NonNull<()>,
    publish: unsafe fn(NonNull<()>, TopicId, ConnectionGeneration, StoredRtdValue) -> XllResult<()>,
    pub(crate) topic_id: TopicId,
    pub(crate) connection_generation: ConnectionGeneration,
}

impl ErasedSink {
    pub(crate) fn for_publish<H: SubscriptionHost>(
        publish: &PublishCore<H>,
        topic_id: TopicId,
        connection_generation: ConnectionGeneration,
    ) -> Self {
        unsafe fn publish_through<H: SubscriptionHost>(
            core: NonNull<()>,
            topic_id: TopicId,
            connection_generation: ConnectionGeneration,
            value: StoredRtdValue,
        ) -> XllResult<()> {
            // SAFETY: `for_publish` records a pointer to `PublishCore<H>`, and
            // the RtdSubscription safety contract requires every sink clone
            // to stop publishing before that server is reclaimed.
            let core = unsafe { core.cast::<PublishCore<H>>().as_ref() };
            core.publish(topic_id, connection_generation, value)
        }

        Self {
            publish_core: NonNull::from(publish).cast(),
            publish: publish_through::<H>,
            topic_id,
            connection_generation,
        }
    }

    #[inline]
    pub(crate) fn publish_stored(&self, value: StoredRtdValue) -> XllResult<()> {
        // SAFETY: construction fixes the erased type and the subscription
        // teardown contract keeps the pointed-to core alive for every call.
        unsafe {
            (self.publish)(
                self.publish_core,
                self.topic_id,
                self.connection_generation,
                value,
            )
        }
    }
}

// SAFETY: PublishCore is Sync and the capability is read-only. Temporal
// validity is guaranteed by the unsafe RtdSubscription contract.
unsafe impl Send for ErasedSink {}
// SAFETY: ErasedSink accesses PublishCore via synchronized atomic/mutex methods.
unsafe impl Sync for ErasedSink {}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VersionedRtdValue {
    pub(crate) generation: ConnectionGeneration,
    pub(crate) sequence: u64,
    pub(crate) value: StoredRtdValue,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ValueSlot {
    Empty,
    Resident(VersionedRtdValue),
    InFlight {
        generation: ConnectionGeneration,
        sequence: u64,
    },
}

impl Default for ValueSlot {
    #[inline]
    fn default() -> Self {
        Self::Empty
    }
}

pub(crate) struct ActiveSubscription {
    pub(crate) id: SubscriptionId,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) committed: bool,
    /// Pending/refresh value slots corresponding to writer buffers 0 and 1.
    pub(crate) values: [ValueSlot; 2],
    /// Index (0 or 1) of the latest value slot for exact publication dedup.
    pub(crate) latest_slot: Option<u8>,
    pub(crate) next_sequence: u64,
    pub(crate) _permit: QuotaPermit,
}

impl ActiveSubscription {
    #[inline]
    pub(crate) fn allocate_sequence(&mut self) -> XllResult<u64> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
        })?;
        Ok(sequence)
    }

    #[inline]
    pub(crate) fn latest_slot_state(&self) -> Option<&ValueSlot> {
        let slot = self.latest_slot? as usize;
        self.values.get(slot)
    }

    #[inline]
    pub(crate) fn latest_value(&self) -> Option<&StoredRtdValue> {
        match self.latest_slot_state()? {
            ValueSlot::Resident(versioned) => Some(&versioned.value),
            ValueSlot::Empty | ValueSlot::InFlight { .. } => None,
        }
    }
}

pub(crate) struct QueuedUpdate {
    pub(crate) connection_generation: ConnectionGeneration,
    pub(crate) sequence: u64,
    pub(crate) _permit: QuotaPermit,
}

pub(crate) struct RtdUpdate {
    pub(crate) sequence: u64,
    pub(crate) topic_id: i32,
    pub(crate) connection_generation: ConnectionGeneration,
    pub(crate) buffer: u8,
    pub(crate) value: StoredRtdValue,
}

/// Immutable control snapshot for one refresh transaction.
///
/// The snapshot contract is independent of collection scheduling:
///
/// - planning and collection never remove a pending update;
/// - shard collection accepts only updates whose generation is still active;
/// - completion retires only the same generation through the delivered sequence;
/// - a newer sequence therefore survives completion of an older snapshot;
/// - collection order is not semantically significant across topics;
/// - each refresh contains at most the newest eligible update for each topic; and
/// - sequence numbers order updates only within one topic generation.
///
/// A publish racing after planning may appear in this refresh or remain indexed
/// for the next one. Either outcome is valid; losing the update is not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RefreshPlan {
    pub(crate) refresh_id: u64,
    pub(crate) epoch: u64,
    /// Advisory work index captured at the control transition. Updates published
    /// after this snapshot may be collected now or remain pending for the next
    /// refresh; the shard-local pending maps remain the source of truth.
    pub(crate) candidate_shards: u32,
}

pub(crate) struct ShardRefreshBatch {
    pub(crate) shard_index: usize,
    pub(crate) updates: Vec<RtdUpdate>,
}

#[cfg(test)]
impl RtdUpdate {
    pub(crate) fn for_test(topic_id: i32, value: RtdValue) -> Self {
        Self {
            sequence: 0,
            topic_id,
            connection_generation: ConnectionGeneration::new(1)
                .expect("test connection generation is non-zero"),
            buffer: 0,
            value: value.into_stored().expect("test RTD value is valid"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefreshOutcome {
    Delivered,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignalState {
    Dormant,
    Calling { ticket: u64, attempt: u8 },
    Signaled { ticket: u64 },
    Suppressed { ticket: u64 },
}

#[derive(Debug)]
pub(crate) enum DeliveryPhase {
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

pub(crate) const TOPIC_SHARDS: usize = 32;

pub(crate) fn shard_index(topic_id: TopicId) -> usize {
    (topic_id.0 as usize) & (TOPIC_SHARDS - 1)
}

pub(crate) const SERVER_LIFECYCLE_OPEN: u8 = 0;
pub(crate) const SERVER_LIFECYCLE_CLOSING: u8 = 1;
pub(crate) const SERVER_LIFECYCLE_TERMINATED: u8 = 2;

#[derive(Default)]
pub(crate) struct TopicShard {
    pub(crate) active_by_topic: FxHashMap<TopicId, ActiveSubscription>,
    pub(crate) topic_by_id: FxHashMap<SubscriptionId, TopicId>,
    pub(crate) pending: [FxHashMap<TopicId, QueuedUpdate>; 2],
    /// Exact number of pending entries belonging to committed connections.
    pub(crate) deliverable_count: usize,
}

pub(crate) struct NotificationAttempt<N> {
    pub(crate) ticket: u64,
    pub(crate) notifier: N,
}

pub(crate) struct PreparedNotification<N> {
    pub(crate) ticket: u64,
    pub(crate) notifier: N,
}

pub(crate) enum NotificationCompletion<N> {
    Finished,
    Retry(NotificationAttempt<N>),
    Failed(XllError),
}

pub(crate) struct RefreshState<N> {
    pub(crate) next_refresh_id: u64,
    pub(crate) next_notification_ticket: u64,
    pub(crate) notifier: Option<N>,
    pub(crate) phase: DeliveryPhase,
}

impl<N> Default for RefreshState<N> {
    fn default() -> Self {
        Self {
            next_refresh_id: 0,
            next_notification_ticket: 0,
            notifier: None,
            phase: DeliveryPhase::default(),
        }
    }
}

impl<N> RefreshState<N> {
    pub(crate) fn ensure_notification_ticket(&self) -> XllResult<()> {
        let ticket = self.next_notification_ticket;
        ticket.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::TICKET_OVERFLOW,
        })?;
        Ok(())
    }

    pub(crate) fn attach_notifier(&mut self, notifier: N) -> Option<N> {
        let retired = self.notifier.replace(notifier);
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    pub(crate) fn detach_notifier(&mut self) -> Option<N> {
        let retired = self.notifier.take();
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    pub(crate) fn commit_notification(
        &mut self,
        prepared: PreparedNotification<N>,
    ) -> NotificationAttempt<N> {
        self.next_notification_ticket = prepared.ticket + 1;
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Calling {
                ticket: prepared.ticket,
                attempt: 0,
            };
        }
        NotificationAttempt {
            ticket: prepared.ticket,
            notifier: prepared.notifier,
        }
    }

    pub(crate) fn signal_calling_mut(&mut self, ticket: u64) -> Option<&mut SignalState> {
        let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase else {
            return None;
        };
        if matches!(signal, SignalState::Calling { ticket: t, .. } if *t == ticket) {
            Some(signal)
        } else {
            None
        }
    }

    pub(crate) fn signal_for_ticket_mut(&mut self, ticket: u64) -> Option<&mut SignalState> {
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

impl<N: Clone> RefreshState<N> {
    pub(crate) fn prepare_notification(
        &self,
        has_pending_updates: bool,
    ) -> XllResult<Option<PreparedNotification<N>>> {
        if !has_pending_updates {
            return Ok(None);
        }
        let DeliveryPhase::BetweenRefreshes { signal } = &self.phase else {
            return Ok(None);
        };
        if !matches!(signal, SignalState::Dormant) {
            return Ok(None);
        }
        let Some(notifier) = self.notifier.as_ref().cloned() else {
            return Ok(None);
        };
        let ticket = self.next_notification_ticket;
        self.ensure_notification_ticket()?;
        Ok(Some(PreparedNotification { ticket, notifier }))
    }
}
