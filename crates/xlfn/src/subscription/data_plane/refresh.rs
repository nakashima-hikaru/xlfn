//! Refresh authority keeps its issuer, admission and rollback buffer private.
use super::*;
use std::panic::AssertUnwindSafe;

#[cfg(any(
    all(target_os = "windows", any(feature = "rtd", feature = "handles")),
    all(test, feature = "rtd")
))]
mod publication;
#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use publication::RefreshOutputSlots;
#[cfg(all(test, target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use publication::write_refresh_data_for_test;

impl<H: SubscriptionHost> PublishCore<H> {
    fn complete_refresh_inner(
        &self,
        refresh_id: u64,
        mut delivered_updates: Vec<RtdUpdate>,
        outcome: RefreshOutcome,
    ) -> XllResult<Option<NotificationAttempt<'_, H>>> {
        let ticket_error = {
            let refresh = self.refresh.lock();
            let DeliveryPhase::Refreshing {
                refresh_id: active_id,
            } = refresh.phase
            else {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::NO_REFERENCE,
                });
            };
            if active_id != refresh_id {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_ID_MISMATCH,
                });
            }
            refresh.ensure_notification_ticket().err()
        };
        // A failed notification reservation must not drop the only owners of
        // InFlight values. Restore the batch first, then report that failure.
        let outcome = if ticket_error.is_some() {
            RefreshOutcome::Failed
        } else {
            outcome
        };

        // Collection groups each shard in dense pending-entry order. Retire
        // backwards so the common full-buffer drain removes tail entries
        // without moving survivors or repairing their indices.
        let mut updates_iter = delivered_updates.drain(..).rev();
        let mut current_opt = updates_iter.next();
        while let Some(first_update) = current_opt {
            let shard_idx = shard_index(TopicId(first_update.topic_id));
            let mut shard = self.shards[shard_idx].lock();
            let mut current = first_update;
            loop {
                let topic_id = TopicId(current.topic_id);
                let slot = current.buffer.index();

                match outcome {
                    RefreshOutcome::Delivered => {
                        if let Some(active) = shard.active_by_topic.get_mut(&topic_id)
                            && active.generation == current.connection_generation
                            && let ValueSlot::InFlight {
                                generation,
                                sequence,
                            } = active.values[slot]
                            && generation == current.connection_generation
                            && sequence == current.sequence
                        {
                            if active.latest_slot == Some(current.buffer) {
                                active.values[slot] = ValueSlot::Resident(VersionedRtdValue {
                                    generation,
                                    sequence,
                                    value: current.value,
                                });
                            } else {
                                active.values[slot] = ValueSlot::Empty;
                            }
                        }

                        self.retire_pending_through(
                            &mut shard,
                            shard_idx,
                            topic_id,
                            current.connection_generation,
                            current.sequence,
                        );
                    }
                    RefreshOutcome::Failed => {
                        if let Some(active) = shard.active_by_topic.get_mut(&topic_id)
                            && active.generation == current.connection_generation
                            && let ValueSlot::InFlight {
                                generation,
                                sequence,
                            } = active.values[slot]
                            && generation == current.connection_generation
                            && sequence == current.sequence
                        {
                            active.values[slot] = ValueSlot::Resident(VersionedRtdValue {
                                generation,
                                sequence,
                                value: current.value,
                            });
                        }
                    }
                }

                match updates_iter.next() {
                    Some(next_update)
                        if shard_index(TopicId(next_update.topic_id)) == shard_idx =>
                    {
                        current = next_update;
                    }
                    other => {
                        current_opt = other;
                        break;
                    }
                }
            }
        }

        drop(updates_iter);
        let mut refresh = self.refresh.lock();
        let DeliveryPhase::Refreshing {
            refresh_id: active_id,
        } = refresh.phase
        else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::NO_REFERENCE,
            });
        };
        if active_id != refresh_id {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_ID_MISMATCH,
            });
        }
        refresh.phase = DeliveryPhase::BetweenRefreshes {
            signal: SignalState::Dormant,
        };
        refresh.recycled_updates = delivered_updates;

        let has_updates = self.has_deliverable_updates();
        if let Some(error) = ticket_error {
            return Err(error);
        }
        self.prepare_notification(&mut refresh, has_updates)
    }

    fn abort_refresh_no_unwind(&self, refresh_id: u64, updates: Vec<RtdUpdate>) {
        if !updates.is_empty() {
            // An unwind during collection can leave an owned partial batch.
            // Restore its InFlight slots before making another refresh
            // admissible, exactly as when a collected batch is dropped.
            if let Ok(Some(attempt)) =
                self.complete_refresh_inner(refresh_id, updates, RefreshOutcome::Failed)
            {
                attempt.drive();
            }
            return;
        }
        let attempt = {
            let mut refresh = self.refresh.lock();
            if let DeliveryPhase::Refreshing {
                refresh_id: active_id,
            } = refresh.phase
            {
                if active_id == refresh_id {
                    refresh.phase = DeliveryPhase::BetweenRefreshes {
                        signal: SignalState::Dormant,
                    };
                    refresh.recycled_updates = updates;
                    let has_updates = self.has_deliverable_updates();
                    self.prepare_notification(&mut refresh, has_updates)
                        .ok()
                        .flatten()
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(attempt) = attempt {
            attempt.drive();
        }
    }

    pub(crate) fn plan_refresh(&self) -> XllResult<PlannedRtdRefresh<'_, H>> {
        let operation = self.enter_operation()?;
        let (plan, updates) = {
            self.ensure_open()?;
            let mut refresh = self.refresh.lock();
            if matches!(refresh.phase, DeliveryPhase::Refreshing { .. }) {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OVERLAPPED_REFERENCE,
                });
            }
            let refresh_id = refresh.next_refresh_id;
            refresh.next_refresh_id = refresh_id.checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
            })?;

            let previous_epoch = self
                .publish_epoch
                .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                    epoch.checked_add(1)
                })
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
                })?;
            let epoch = previous_epoch + 1;
            refresh.phase = DeliveryPhase::Refreshing { refresh_id };
            (
                RefreshPlan {
                    refresh_id,
                    epoch,
                    candidate_shards: self.ready_shards.load(Ordering::Acquire),
                    estimated_updates: self.deliverable_pending.load(Ordering::Acquire),
                },
                std::mem::take(&mut refresh.recycled_updates),
            )
        };
        Ok(PlannedRtdRefresh {
            publish: self,
            operation: Some(operation),
            plan,
            updates,
            finished: false,
        })
    }

    fn collect_refresh(&self, plan: &RefreshPlan, updates: &mut Vec<RtdUpdate>) {
        debug_assert_eq!(self.publish_epoch.load(Ordering::Acquire), plan.epoch);
        debug_assert!(updates.is_empty());
        updates.reserve(plan.estimated_updates);
        let mut shards = plan.candidate_shards;
        while shards != 0 {
            let index = shards.trailing_zeros() as usize;
            shards &= shards - 1;
            self.collect_shard_into(index, updates);
        }
    }
}

#[must_use]
pub(crate) struct PlannedRtdRefresh<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
    operation: Option<ScopedPublishOperation<'a, H>>,
    plan: RefreshPlan,
    updates: Vec<RtdUpdate>,
    finished: bool,
}

impl<'a, H: SubscriptionHost> PlannedRtdRefresh<'a, H> {
    pub(crate) fn collect(mut self) -> RtdRefreshBatch<'a, H> {
        self.publish.collect_refresh(&self.plan, &mut self.updates);
        self.finished = true;
        RtdRefreshBatch {
            publish: self.publish,
            _operation: self.operation.take(),
            plan: self.plan,
            updates: std::mem::take(&mut self.updates),
            finished: false,
        }
    }
}

impl<H: SubscriptionHost> Drop for PlannedRtdRefresh<'_, H> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
            self.publish
                .abort_refresh_no_unwind(self.plan.refresh_id, std::mem::take(&mut self.updates));
        }));
    }
}

#[must_use]
pub(crate) struct RtdRefreshBatch<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
    // Keep admission live through protocol completion and release it before
    // the remaining fields, without copying the guard out of the batch.
    _operation: Option<ScopedPublishOperation<'a, H>>,
    plan: RefreshPlan,
    updates: Vec<RtdUpdate>,
    finished: bool,
}

impl<H: SubscriptionHost> RtdRefreshBatch<'_, H> {
    pub(crate) fn updates(&self) -> &[RtdUpdate] {
        &self.updates
    }

    #[inline]
    fn finish_protocol(&mut self, outcome: RefreshOutcome) -> XllResult<()> {
        self.finished = true;
        let updates = std::mem::take(&mut self.updates);
        let attempt =
            self.publish
                .complete_refresh_inner(self.plan.refresh_id, updates, outcome)?;
        if let Some(attempt) = attempt {
            attempt.drive();
        }
        Ok(())
    }

    #[cfg(any(
        all(target_os = "windows", any(feature = "rtd", feature = "handles")),
        test,
        feature = "bench-internals"
    ))]
    #[inline]
    fn complete_inner(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        // Normal field destruction releases admission on both paths. Keep
        // the guard in place instead of moving it out just before dropping
        // this completed batch; _operation is its first owning field.
        self.finish_protocol(outcome)?;
        Ok(())
    }

    #[cfg(any(
        all(target_os = "windows", any(feature = "rtd", feature = "handles")),
        test,
        feature = "bench-internals"
    ))]
    #[inline]
    pub(crate) fn abort(self) -> XllResult<()> {
        self.complete_inner(RefreshOutcome::Failed)
    }

    /// Explicit simulation support. Production local delivery requires the
    /// concrete marshalled-array handoff; raw outcome selection is test/bench only.
    #[cfg(any(test, feature = "bench-internals"))]
    #[inline]
    pub(crate) fn complete(self, outcome: RefreshOutcome) -> XllResult<()> {
        self.complete_inner(outcome)
    }

    // Keep unwind containment and rollback off the completed batch's Drop
    // path so its already-empty fields can be eliminated by the caller.
    #[cold]
    #[inline(never)]
    fn abort_on_drop(&mut self) {
        let _ = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
            self.finish_protocol(RefreshOutcome::Failed)
        }));
    }
}

// AUDIT [Lock Safety & Self-Deadlock Prevention]:
// `RtdRefreshBatch::drop` acquires shard mutexes during rollback (`complete_refresh_inner`).
// Self-deadlock is structurally impossible because:
// 1. `RtdRefreshBatch` is only constructed via `PlannedRtdRefresh::collect()`,
//    which executes strictly after all `collect_shard` mutex guards have been dropped. No shard
//    lock is held when `RtdRefreshBatch` is handed to the caller.
// 2. Caller code (e.g. `IRtdServer::RefreshData`) and safe consumer APIs never acquire or hold
//    any shard mutexes.
// 3. Stack unwinding from panics in caller code or formatting therefore drops `RtdRefreshBatch`
//    with zero shard mutexes held by the current thread.
// 4. In `complete_refresh_inner`, shard mutexes are acquired sequentially and dropped
//    immediately without holding any other shard lock or the `refresh` lock.
impl<H: SubscriptionHost> Drop for RtdRefreshBatch<'_, H> {
    #[inline]
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.abort_on_drop();
    }
}

#[cfg(all(test, feature = "rtd"))]
mod tests {
    use crate::subscription::runtime::SubscriptionRuntime;
    #[test]
    fn ticket_exhaustion_restores_collected_values_before_returning_error() {
        use crate::subscription::runtime::SubscriptionRuntime;
        use crate::subscription::tests::SourceFixture;
        use crate::subscription::{RtdTopic, StoredRtdValue, TopicId};
        let fixture = SourceFixture::new();
        let (source, sink, _) = fixture.add::<i32>(None);
        let runtime = SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost, _>::with_sources_for_internal(fixture.finish());
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(
                &source,
                RtdTopic::single("ticket-recovery").unwrap().borrowed(),
            )
            .unwrap();
        let id = prepared.id();
        prepared.commit();
        runtime
            .connect_transaction(&server, TopicId(1), id)
            .unwrap()
            .commit()
            .unwrap();
        sink.lock().as_ref().unwrap().publish(42).unwrap();
        let publish = &server.test_server().publish;
        let batch = publish.begin_refresh().unwrap();
        assert_eq!(batch.updates()[0].value, StoredRtdValue::Integer(42));
        publish.refresh.lock().next_notification_ticket = u64::MAX;
        assert!(batch.complete(super::RefreshOutcome::Delivered).is_err());
        // Only the failed reservation is repaired in this test. The rollback
        // itself must have restored the owned value and pending registration.
        publish.refresh.lock().next_notification_ticket = 1;
        let retry = publish.begin_refresh().unwrap();
        assert_eq!(retry.updates().len(), 1);
        assert_eq!(retry.updates()[0].value, StoredRtdValue::Integer(42));
        retry.complete(super::RefreshOutcome::Delivered).unwrap();
        assert_eq!(publish.pending_update_count(), 0);
    }

    #[test]
    fn partial_collection_unwind_restores_in_flight_values() {
        use crate::subscription::tests::SourceFixture;
        use crate::subscription::{RefreshOutcome, RtdTopic, StoredRtdValue, TopicId};
        let fixture = SourceFixture::new();
        let sources = [fixture.add::<i32>(None), fixture.add::<i32>(None)];
        let runtime =
            SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost, _>::with_sources_for_internal(
                fixture.finish(),
            );
        let server = runtime.register_test_server(1);
        for (index, (source, sink, _)) in sources.iter().enumerate() {
            let prepared = runtime
                .prepare(
                    source,
                    RtdTopic::single(format!("partial-{index}"))
                        .unwrap()
                        .borrowed(),
                )
                .unwrap();
            let id = prepared.id();
            prepared.commit();
            runtime
                .connect_transaction(&server, TopicId(index as i32 + 1), id)
                .unwrap()
                .commit()
                .unwrap();
            sink.lock().as_ref().unwrap().publish(index as i32).unwrap();
        }
        let publish = &server.test_server().publish;
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut planned = publish.plan_refresh().unwrap();
                publish.collect_shard_into(1, &mut planned.updates);
                assert_eq!(planned.updates.len(), 1);
                panic!("injected unwind between shard collections");
            }))
            .is_err()
        );
        let retry = server.begin_refresh().unwrap();
        let mut values = retry
            .updates()
            .iter()
            .map(|update| (update.topic_id, update.value.clone()))
            .collect::<Vec<_>>();
        values.sort_by_key(|&(topic, _)| topic);
        assert_eq!(
            values,
            [
                (1, StoredRtdValue::Integer(0)),
                (2, StoredRtdValue::Integer(1)),
            ]
        );
        retry.complete(RefreshOutcome::Delivered).unwrap();
        assert_eq!(server.pending_update_count(), 0);
        assert_eq!(publish.queued_update_count(), 0);
    }
}
