//! Issuer-bound notification authority; tickets are observations, not issuers.
use super::*;
use crate::subscription::delivery::{
    NotificationAttempt as RawNotificationAttempt, NotificationCompletion,
};
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

pub(crate) struct NotificationAttempt<'a, H: SubscriptionHost> {
    pending: RollbackGuard<PendingNotification<'a, H>>,
    attempt: RawNotificationAttempt<H::Notifier>,
}

struct PendingNotification<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
    ticket: u64,
}

impl<H: SubscriptionHost> Rollback for PendingNotification<'_, H> {
    fn rollback(self) {
        let mut refresh = self.publish.refresh.lock();
        if let Some((signal, _)) = refresh.signal_calling_mut(self.ticket) {
            self.publish.invalidate_notified_epoch();
            *signal = SignalState::Dormant;
        }
    }
}

impl<H: SubscriptionHost> PublishCore<H> {
    pub(super) fn invalidate_notified_epoch(&self) {
        self.notified_epoch.swap(u64::MAX, Ordering::AcqRel);
    }

    pub(super) fn was_notified(&self, epoch: u64) -> bool {
        epoch != u64::MAX && self.notified_epoch.load(Ordering::Relaxed) == epoch
    }

    pub(super) fn prepare_notification_for_known_update(
        &self,
        epoch: u64,
    ) -> XllResult<Option<NotificationAttempt<'_, H>>> {
        // A successful RMW orders this committed update before invalidation
        // by the next refresh. That refresh acquires the update before
        // sampling ready shards. If invalidation has won, arbitrate with
        // the refresh under its mutex instead of using the old notification.
        if self.was_notified(epoch)
            && self
                .notified_epoch
                .compare_exchange(epoch, epoch, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            return Ok(None);
        }
        let mut refresh = self.refresh.lock();
        self.prepare_notification(&mut refresh, self.has_deliverable_updates())
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
            let attempt = refresh.commit_notification(prepared);
            let ticket = attempt.ticket();
            self.notified_epoch.store(epoch, Ordering::Release);
            NotificationAttempt {
                pending: RollbackGuard::new(PendingNotification {
                    publish: self,
                    ticket,
                }),
                attempt,
            }
        }))
    }

    fn finish_notification_attempt(
        &self,
        ticket: u64,
        outcome: XllResult<()>,
    ) -> NotificationCompletion {
        let mut refresh = self.refresh.lock();
        let has_notifier = refresh.notifier.is_some();
        let Some((signal, attempt)) = refresh.signal_calling_mut(ticket) else {
            return NotificationCompletion::Finished;
        };
        match outcome {
            Ok(()) => {
                *signal = SignalState::Signaled { ticket };
                NotificationCompletion::Finished
            }
            Err(error) if !has_notifier => {
                self.invalidate_notified_epoch();
                *signal = SignalState::Dormant;
                NotificationCompletion::Failed(error)
            }
            // Attach/detach invalidates the existing ticket, so a matching
            // retry still belongs to the originally reserved notifier.
            Err(_) if attempt < 2 => {
                *signal = SignalState::Calling {
                    ticket,
                    attempt: attempt + 1,
                };
                NotificationCompletion::Retry
            }
            Err(error) => {
                *signal = SignalState::Suppressed { ticket };
                NotificationCompletion::Failed(error)
            }
        }
    }
}

impl<H: SubscriptionHost> NotificationAttempt<'_, H> {
    pub(crate) fn drive(self) {
        let Self { pending, attempt } = self;
        let publish = pending.get().publish;
        let ticket = pending.get().ticket;
        loop {
            // Refresh and notifier replacement may supersede a reserved
            // notice before the driver gets to it. Notifications already in
            // flight can still finish, but cannot mutate a successor ticket.
            if publish.refresh.lock().signal_calling_mut(ticket).is_none() {
                break;
            }
            let _callback = ServerCallbackObservation::begin(publish.services());
            let res = catch_unwind(AssertUnwindSafe(|| publish.host.notify(attempt.notifier())));
            let completion = match res {
                Ok(outcome) => publish.finish_notification_attempt(ticket, outcome),
                Err(panic_payload) => {
                    drop(pending);
                    let err = crate::error::construct::internal(
                        crate::diagnostics::catalog::PANIC_NOTIFY,
                    );
                    publish.services().record_cleanup_result(Err(err));
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                NotificationCompletion::Finished => break,
                NotificationCompletion::Retry => continue,
                NotificationCompletion::Failed(err) => {
                    publish.services().record_cleanup_result(Err(err));
                    break;
                }
            }
        }
        pending.complete();
    }
}

#[cfg(all(test, not(miri), not(all(target_os = "windows", target_arch = "x86"))))]
#[test]
fn loom_notification_invalidation_hands_late_updates_to_a_refresh_or_publisher() {
    use loom::sync::atomic::{AtomicBool, AtomicU64};
    use loom::sync::{Arc, Mutex};

    loom::model(|| {
        let state = Arc::new((
            AtomicU64::new(0),
            AtomicBool::new(false),
            Mutex::new((false, false)),
        ));
        let writer_state = Arc::clone(&state);
        let writer = loom::thread::spawn(move || {
            let (notified, ready, refresh) = &*writer_state;
            ready.store(true, Ordering::Relaxed);
            if notified.load(Ordering::Relaxed) != 0
                || notified
                    .compare_exchange(0, 0, Ordering::AcqRel, Ordering::Relaxed)
                    .is_err()
            {
                let mut refresh = refresh.lock().unwrap();
                if !refresh.0 {
                    refresh.1 = true;
                }
            }
        });

        let (notified, ready, refresh) = &*state;
        let collected = {
            let mut refresh = refresh.lock().unwrap();
            notified.swap(u64::MAX, Ordering::AcqRel);
            refresh.0 = true;
            ready.load(Ordering::Acquire)
        };
        {
            let mut refresh = refresh.lock().unwrap();
            refresh.0 = false;
            refresh.1 = ready.load(Ordering::Acquire);
        }
        writer.join().unwrap();
        assert!(collected || refresh.lock().unwrap().1);
    });
}
