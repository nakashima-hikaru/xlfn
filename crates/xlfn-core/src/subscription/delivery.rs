use super::*;
use rustc_hash::FxHashMap;

#[derive(Clone, Debug)]
pub(crate) struct ErasedSink {
    pub(crate) server: Weak<ServerRuntime>,
    pub(crate) topic_id: TopicId,
    pub(crate) connection_generation: ConnectionGeneration,
}

impl ErasedSink {
    pub(crate) fn publish(&self, value: RtdValue) -> XllResult<()> {
        let server = self.server.upgrade().ok_or(XllError::Closing)?;
        server.publish(self.topic_id, self.connection_generation, value)
    }
}

pub(crate) struct ActiveSubscription {
    pub(crate) key: SubscriptionKey,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) subscription: Option<Box<dyn RtdSubscription>>,
    pub(crate) committed: bool,
    pub(crate) latest: Arc<RtdValue>,
    pub(crate) _permit: QuotaPermit,
}

pub(crate) struct QueuedUpdate {
    pub(crate) connection_generation: ConnectionGeneration,
    pub(crate) sequence: u64,
    pub(crate) value: Arc<RtdValue>,
    pub(crate) _permit: QuotaPermit,
}

pub(crate) struct RtdUpdate {
    pub(crate) sequence: u64,
    pub(crate) topic_id: i32,
    pub(crate) value: Arc<RtdValue>,
}

#[cfg(test)]
impl RtdUpdate {
    pub(crate) fn for_test(topic_id: i32, value: RtdValue) -> Self {
        Self {
            sequence: 0,
            topic_id,
            value: Arc::new(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefreshOutcome {
    Delivered,
    Failed,
}

pub(crate) type NotificationCallback = Arc<dyn Fn() -> XllResult<()> + Send + Sync>;

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

#[derive(Clone)]
pub(crate) struct NotificationAttempt {
    pub(crate) ticket: u64,
    pub(crate) callback: NotificationCallback,
}

pub(crate) struct PreparedNotification {
    pub(crate) ticket: u64,
    pub(crate) callback: NotificationCallback,
}

pub(crate) enum NotificationCompletion {
    Finished,
    Retry(NotificationAttempt),
    Failed(XllError),
}

#[derive(Default)]
pub(crate) struct RefreshState {
    pub(crate) next_refresh_id: u64,
    pub(crate) next_notification_ticket: u64,
    pub(crate) callback: Option<NotificationCallback>,
    pub(crate) phase: DeliveryPhase,
}

impl RefreshState {
    pub(crate) fn ensure_notification_ticket(&self) -> XllResult<()> {
        let ticket = self.next_notification_ticket;
        ticket.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: 0x5449_434b_4f56_464c,
        })?;
        Ok(())
    }

    pub(crate) fn attach_callback(
        &mut self,
        callback: NotificationCallback,
    ) -> Option<NotificationCallback> {
        let retired = self.callback.replace(callback);
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    pub(crate) fn detach_callback(&mut self) -> Option<NotificationCallback> {
        let retired = self.callback.take();
        if let DeliveryPhase::BetweenRefreshes { signal } = &mut self.phase {
            *signal = SignalState::Dormant;
        }
        retired
    }

    pub(crate) fn prepare_notification(
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
        self.ensure_notification_ticket()?;
        Ok(Some(PreparedNotification { ticket, callback }))
    }

    pub(crate) fn commit_notification(
        &mut self,
        prepared: PreparedNotification,
    ) -> NotificationAttempt {
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
