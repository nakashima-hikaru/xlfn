use super::*;
use rustc_hash::FxHashMap;

#[derive(Clone, Debug)]
pub(crate) struct ErasedSink {
    pub(crate) publish: triomphe::Arc<PublishCore>,
    pub(crate) topic_id: TopicId,
    pub(crate) connection_generation: ConnectionGeneration,
}

impl ErasedSink {
    #[inline]
    pub(crate) fn publish(&self, value: RtdValue) -> XllResult<()> {
        self.publish
            .publish(self.topic_id, self.connection_generation, value)
    }
}

pub(crate) struct ActiveSubscription {
    pub(crate) key: SubscriptionKey,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) committed: bool,
    pub(crate) latest: StoredRtdValue,
    pub(crate) _permit: QuotaPermit,
}

pub(crate) struct QueuedUpdate {
    pub(crate) connection_generation: ConnectionGeneration,
    pub(crate) sequence: u64,
    pub(crate) value: StoredRtdValue,
    pub(crate) _permit: QuotaPermit,
}

pub(crate) struct RtdUpdate {
    pub(crate) sequence: u64,
    pub(crate) topic_id: i32,
    pub(crate) value: StoredRtdValue,
}

#[cfg(test)]
impl RtdUpdate {
    pub(crate) fn for_test(topic_id: i32, value: RtdValue) -> Self {
        Self {
            sequence: 0,
            topic_id,
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
    pub(crate) topic_by_key: FxHashMap<SubscriptionKey, TopicId>,
    pub(crate) pending: [FxHashMap<TopicId, QueuedUpdate>; 2],
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
            diagnostic_id: crate::DiagnosticId::TICKET_OVERFLOW,
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
