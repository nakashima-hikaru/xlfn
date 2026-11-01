//! Issuer-bound notification authority; tickets are observations, not issuers.
use super::*;
use crate::subscription::delivery::{
    NotificationAttempt as RawNotificationAttempt,
    NotificationCompletion as RawNotificationCompletion,
};
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;

pub(crate) struct NotificationAttempt<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
    attempt: RawNotificationAttempt<H::Notifier>,
}

impl<H: SubscriptionHost> PublishCore<H> {
    pub(super) fn prepare_notification_for_known_update(
        &self,
        epoch: u64,
    ) -> XllResult<Option<NotificationAttempt<'_, H>>> {
        if self.notified_epoch.load(Ordering::Acquire) == epoch {
            return Ok(None);
        }
        let mut refresh = self.refresh.lock();
        self.prepare_notification(&mut refresh, true)
    }

    /// Reserves a notification and publishes its fast-path epoch together.
    /// The caller holds `refresh`, which also serializes epoch advancement.
    /// A publisher may arrive with an older epoch after waiting for this lock;
    /// cache the current epoch of the reserved notification, not that snapshot.
    pub(super) fn prepare_notification(
        &self,
        refresh: &mut RefreshState<H::Notifier>,
        has_updates: bool,
    ) -> XllResult<Option<NotificationAttempt<'_, H>>> {
        let prepared = refresh.prepare_notification(has_updates)?;
        Ok(prepared.map(|prepared| {
            let epoch = self.publish_epoch.load(Ordering::Acquire);
            self.notified_epoch.store(epoch, Ordering::Release);
            NotificationAttempt {
                publish: self,
                attempt: refresh.commit_notification(prepared),
            }
        }))
    }

    fn finish_notification_attempt(
        &self,
        original: RawNotificationAttempt<H::Notifier>,
        outcome: XllResult<()>,
    ) -> RawNotificationCompletion<H::Notifier> {
        let ticket = original.ticket();
        let mut refresh = self.refresh.lock();
        let notifier = refresh.notifier.clone();
        let Some(signal) = refresh.signal_for_ticket_mut(ticket) else {
            return RawNotificationCompletion::Finished;
        };

        let attempt = match signal {
            SignalState::Calling { ticket: t, attempt } if *t == ticket => *attempt,
            _ => return RawNotificationCompletion::Finished,
        };

        match outcome {
            Ok(()) => {
                if let Some(signal) = refresh.signal_calling_mut(ticket) {
                    *signal = SignalState::Signaled { ticket };
                }
                RawNotificationCompletion::Finished
            }
            Err(error) => {
                if attempt < 2 {
                    let next_attempt = attempt + 1;
                    if let Some(signal) = refresh.signal_calling_mut(ticket) {
                        *signal = SignalState::Calling {
                            ticket,
                            attempt: next_attempt,
                        };
                    }
                    if let Some(notifier) = notifier {
                        RawNotificationCompletion::Retry(original.with_notifier(notifier))
                    } else {
                        if let DeliveryPhase::BetweenRefreshes {
                            signal: signal @ SignalState::Suppressed { .. },
                        } = &mut refresh.phase
                        {
                            *signal = SignalState::Dormant;
                        }
                        RawNotificationCompletion::Failed(error)
                    }
                } else {
                    if let Some(signal) = refresh.signal_calling_mut(ticket) {
                        *signal = SignalState::Suppressed { ticket };
                    }
                    RawNotificationCompletion::Failed(error)
                }
            }
        }
    }
}

impl<H: SubscriptionHost> NotificationAttempt<'_, H> {
    pub(crate) fn drive(self) {
        let Self {
            publish,
            mut attempt,
        } = self;
        loop {
            let _callback = ServerCallbackObservation::begin(publish.services());
            let res = catch_unwind(AssertUnwindSafe(|| publish.host.notify(attempt.notifier())));
            let completion = match res {
                Ok(Ok(())) => publish.finish_notification_attempt(attempt, Ok(())),
                Ok(Err(err)) => publish.finish_notification_attempt(attempt, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_NOTIFY,
                    };
                    publish.services().record_cleanup_result(Err(err.clone()));
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                RawNotificationCompletion::Finished => break,
                RawNotificationCompletion::Retry(next) => attempt = next,
                RawNotificationCompletion::Failed(err) => {
                    publish.services().record_cleanup_result(Err(err));
                    break;
                }
            }
        }
    }
}
