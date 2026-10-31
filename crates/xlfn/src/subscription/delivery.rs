#![allow(
    unsafe_code,
    reason = "PublishSink is an audited non-owning capability over a runtime-owned PublishCore"
)]

use super::data_plane::PublishCore;
use super::topic::{SubscriptionId, TopicId};
#[cfg(test)]
use super::value::RtdValue;
use super::value::StoredRtdValue;
use crate::excel_rtd::RtdSubscriptionHost;
use crate::generation::ConnectionGeneration;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::ptr::NonNull;
use xlfn_kernel::quota::DetachedQuotaPermit;

#[cfg(feature = "rtd")]
pub(crate) type PublishBatch = smallvec::SmallVec<[StoredRtdValue; 32]>;

/// Non-owning capability into the concrete production publication backend.
/// Source values remain generic; the host type is retained through each call.
#[derive(Clone, Copy)]
pub(crate) struct PublishSink {
    publish_core: NonNull<PublishCore<RtdSubscriptionHost>>,
    pub(crate) topic_id: TopicId,
    pub(crate) connection_generation: ConnectionGeneration,
}

impl PublishSink {
    pub(crate) fn for_publish(
        publish: &PublishCore<RtdSubscriptionHost>,
        topic_id: TopicId,
        connection_generation: ConnectionGeneration,
    ) -> Self {
        Self {
            publish_core: NonNull::from(publish),
            topic_id,
            connection_generation,
        }
    }

    #[inline]
    fn core(&self) -> &PublishCore<RtdSubscriptionHost> {
        // SAFETY: the subscription's sink-transfer contract retains this
        // concrete core until all of its scalar and batch publishers stop.
        unsafe { self.publish_core.as_ref() }
    }

    #[cfg(feature = "rtd")]
    pub(super) fn channel_budget(&self) -> triomphe::Arc<super::budget::ChannelBudget> {
        triomphe::Arc::clone(&self.core().services().channel_budget)
    }

    #[cfg(feature = "rtd")]
    pub(super) fn publisher_queue(
        &self,
    ) -> XllResult<triomphe::Arc<super::channel::PublisherQueue>> {
        self.core().services().publishers.queue()
    }

    #[inline]
    pub(crate) fn publish_stored(&self, value: StoredRtdValue) -> XllResult<()> {
        self.core()
            .publish(self.topic_id, self.connection_generation, value)
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn publish_batch(
        &self,
        values: PublishBatch,
        stopping: &std::sync::atomic::AtomicBool,
    ) -> XllResult<()> {
        self.core()
            .publish_batch(self.topic_id, self.connection_generation, values, stopping)
    }
}

// SAFETY: the concrete PublishCore is Sync and all access is synchronized.
// The unsafe RtdSubscription contract retains it across every sink user.
unsafe impl Send for PublishSink {}
// SAFETY: shared access only calls the synchronized production backend.
unsafe impl Sync for PublishSink {}

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
    pub(crate) _permit: DetachedQuotaPermit,
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
    pub(crate) _permit: DetachedQuotaPermit,
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
    /// Advisory capacity estimate captured at the control transition. Concurrent
    /// publishes may make this estimate stale without affecting correctness.
    pub(crate) estimated_updates: usize,
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
    // Dense entries keep refresh traversal proportional to current updates,
    // even after a full snapshot leaves large retained hash-table capacities.
    // Cross-topic order is unspecified, so completion uses swap removal.
    pub(crate) pending: [indexmap::IndexMap<TopicId, QueuedUpdate, rustc_hash::FxBuildHasher>; 2],
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
    /// Empty storage returned by the previous completed refresh transaction.
    pub(crate) recycled_updates: Vec<RtdUpdate>,
}

impl<N> Default for RefreshState<N> {
    fn default() -> Self {
        Self {
            next_refresh_id: 0,
            next_notification_ticket: 0,
            notifier: None,
            phase: DeliveryPhase::default(),
            recycled_updates: Vec::new(),
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

#[cfg(all(test, feature = "rtd"))]
mod typed_sink_tests {
    use super::*;
    use crate::rtd::test_support::TestNotifierState;
    use crate::subscription::host::SubscriptionHost;
    use crate::subscription::runtime::SubscriptionRuntime;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn miri_typed_sink_uses_production_backend_for_scalar_and_batch() {
        let runtime = SubscriptionRuntime::<RtdSubscriptionHost>::new();
        let server = runtime.register_test_server(1);
        let core = server.test_server().publish.as_ref();
        let topic = TopicId(1);
        let generation = ConnectionGeneration::new(1).unwrap();
        core.reserve_connection(topic, SubscriptionId(1), generation)
            .unwrap();
        core.install_connection(topic, generation).unwrap();
        assert!(
            core.commit_connection(topic, generation, None)
                .unwrap()
                .is_none()
        );
        let notifier = Arc::new(TestNotifierState::new());
        server
            .attach_update_notifier(crate::excel_rtd::RtdNotifier::for_test(Arc::clone(
                &notifier,
            )))
            .unwrap();

        // This is the exact production adapter, with no cfg(test) fallback or
        // untyped core pointer. Both publication methods target the same core.
        let sink = RtdSubscriptionHost::publish_sink(core, topic, generation);
        let expected: NonNull<PublishCore<RtdSubscriptionHost>> = NonNull::from(core);
        assert_eq!(sink.publish_core, expected);
        sink.publish_stored(StoredRtdValue::Integer(7)).unwrap();
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(7));
        batch.complete(RefreshOutcome::Delivered).unwrap();

        let stopping = AtomicBool::new(false);
        sink.publish_batch(
            smallvec::smallvec![StoredRtdValue::Integer(8), StoredRtdValue::Integer(9)],
            &stopping,
        )
        .unwrap();
        let batch = server.begin_refresh().unwrap();
        assert_eq!(batch.updates.len(), 1);
        assert_eq!(batch.updates[0].value, StoredRtdValue::Integer(9));
        batch.complete(RefreshOutcome::Delivered).unwrap();
        assert!(notifier.calls.load(Ordering::Acquire) >= 2);

        let stale =
            RtdSubscriptionHost::publish_sink(core, topic, ConnectionGeneration::new(2).unwrap());
        assert!(matches!(
            stale.publish_stored(StoredRtdValue::Integer(10)),
            Err(XllError::Closing)
        ));
        assert!(matches!(
            stale.publish_batch(smallvec::smallvec![StoredRtdValue::Integer(10)], &stopping),
            Err(XllError::Closing)
        ));
        assert_eq!(server.pending_update_count(), 0);

        stopping.store(true, Ordering::Release);
        sink.publish_batch(smallvec::smallvec![StoredRtdValue::Integer(11)], &stopping)
            .unwrap();
        assert_eq!(server.pending_update_count(), 0);
        stopping.store(false, Ordering::Release);
        server.terminate().unwrap();
        // The runtime still owns this terminal core. Closed admission must
        // reject both direct adapter paths before looking up a retired topic.
        assert!(matches!(
            sink.publish_stored(StoredRtdValue::Integer(12)),
            Err(XllError::Closing)
        ));
        assert!(matches!(
            sink.publish_batch(smallvec::smallvec![StoredRtdValue::Integer(12)], &stopping),
            Err(XllError::Closing)
        ));
        assert_eq!(runtime.services.active_quota.used(), 0);
        assert_eq!(runtime.services.queued_update_quota.used(), 0);
        runtime.close().unwrap();
    }
}
