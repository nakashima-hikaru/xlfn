use super::identity::SubscriptionIdentityIndex;
use super::source::SourceHandleId;
use super::topic::{
    RtdLimits, RtdTopic, SourceId, SubscriptionId, SubscriptionIdentity, SubscriptionKey,
};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;

use xlfn_kernel::invariant::checked_sub_or_abort;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingCommitment {
    Uncommitted,
    Committed,
}

pub(crate) struct ActiveReservation {
    reservations: NonZeroUsize,
}

pub(crate) enum SubscriptionPhase {
    Pending {
        reservations: Option<NonZeroUsize>,
        server: Option<ServerGeneration>,
        commitment: PendingCommitment,
    },
    Connecting {
        reservations: Option<NonZeroUsize>,
        server: ServerGeneration,
        connection: ConnectionGeneration,
        commitment: PendingCommitment,
    },
    Active {
        server: ServerGeneration,
        connection: ConnectionGeneration,
        reservation: Option<ActiveReservation>,
    },
}

pub(crate) enum PreparationFinish {
    Keep,
    Remove,
}

pub(crate) struct SubscriptionEntry {
    pub(crate) source_id: SourceId,
    pub(crate) topic: RtdTopic,
    pub(crate) phase: SubscriptionPhase,
}

impl SubscriptionEntry {
    pub(crate) fn is_connected(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Connecting { .. } | SubscriptionPhase::Active { .. }
        )
    }

    pub(crate) fn is_active(&self) -> bool {
        matches!(self.phase, SubscriptionPhase::Active { .. })
    }

    pub(crate) fn tracks_pending_bytes(&self) -> bool {
        !matches!(
            self.phase,
            SubscriptionPhase::Active {
                reservation: None,
                ..
            }
        )
    }

    pub(crate) fn can_remove(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Pending {
                reservations: None,
                commitment: PendingCommitment::Uncommitted,
                ..
            }
        )
    }

    pub(crate) fn server_generation(&self) -> Option<ServerGeneration> {
        match &self.phase {
            SubscriptionPhase::Pending { server, .. } => *server,
            SubscriptionPhase::Connecting { server, .. }
            | SubscriptionPhase::Active { server, .. } => Some(*server),
        }
    }

    pub(crate) fn connection_generation(&self) -> Option<ConnectionGeneration> {
        match &self.phase {
            SubscriptionPhase::Pending { .. } => None,
            SubscriptionPhase::Connecting { connection, .. }
            | SubscriptionPhase::Active { connection, .. } => Some(*connection),
        }
    }

    pub(crate) fn add_reservation(&mut self) -> XllResult<()> {
        let reservations = match &mut self.phase {
            SubscriptionPhase::Pending { reservations, .. }
            | SubscriptionPhase::Connecting { reservations, .. } => reservations,
            SubscriptionPhase::Active { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }
        };
        let next = reservations
            .map_or(0, NonZeroUsize::get)
            .checked_add(1)
            .ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RESERVATION_OVERFLOW,
            })?;
        *reservations = Some(NonZeroUsize::new(next).expect("reservation count is non-zero"));
        Ok(())
    }

    pub(crate) fn finish_preparation(&mut self, committed: bool) -> PreparationFinish {
        match &mut self.phase {
            SubscriptionPhase::Pending {
                reservations,
                commitment,
                ..
            }
            | SubscriptionPhase::Connecting {
                reservations,
                commitment,
                ..
            } => {
                if committed {
                    *commitment = PendingCommitment::Committed;
                }
                if let Some(current) = reservations {
                    if current.get() == 1 {
                        *reservations = None;
                    } else {
                        *current = NonZeroUsize::new(current.get() - 1)
                            .expect("a remaining reservation is non-zero");
                    }
                }
                if !committed && self.can_remove() {
                    PreparationFinish::Remove
                } else {
                    PreparationFinish::Keep
                }
            }
            SubscriptionPhase::Active { reservation, .. } => {
                let Some(active) = reservation else {
                    return PreparationFinish::Keep;
                };
                if active.reservations.get() == 1 {
                    *reservation = None;
                    PreparationFinish::Keep
                } else {
                    active.reservations = NonZeroUsize::new(active.reservations.get() - 1)
                        .expect("a remaining reservation is non-zero");
                    PreparationFinish::Keep
                }
            }
        }
    }

    pub(crate) fn claim_server(&mut self, generation: ServerGeneration) -> XllResult<()> {
        let server = match &mut self.phase {
            SubscriptionPhase::Pending { server, .. } => server,
            SubscriptionPhase::Connecting { server, .. }
            | SubscriptionPhase::Active { server, .. } => {
                if *server != generation {
                    return Err(XllError::Internal {
                        diagnostic_id:
                            crate::diagnostics::id::DiagnosticId::SERVER_GENERATION_MISMATCH,
                    });
                }
                return Ok(());
            }
        };
        if let Some(existing) = *server {
            if existing != generation {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::SERVER_GENERATION_MISMATCH,
                });
            }
        } else {
            *server = Some(generation);
        }
        Ok(())
    }

    pub(crate) fn begin_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> XllResult<()> {
        let (reservations, commitment, existing_server) = match &self.phase {
            SubscriptionPhase::Pending {
                reservations,
                commitment,
                server,
            } => (*reservations, *commitment, *server),
            SubscriptionPhase::Connecting { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CONNECTION_INFLIGHT,
                });
            }
            SubscriptionPhase::Active { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }
        };
        if let Some(existing) = existing_server
            && existing != server
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::SERVER_GENERATION_MISMATCH,
            });
        }
        self.phase = SubscriptionPhase::Connecting {
            reservations,
            server,
            connection,
            commitment,
        };
        Ok(())
    }

    pub(crate) fn rollback_connection(&mut self, connection: ConnectionGeneration) -> bool {
        let (reservations, server, commitment) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection: current,
                commitment,
            } if *current == connection => (*reservations, *server, *commitment),
            _ => return false,
        };
        self.phase = SubscriptionPhase::Pending {
            reservations,
            server: Some(server),
            commitment,
        };
        true
    }

    pub(crate) fn finish_connection(&mut self, connection: ConnectionGeneration) -> bool {
        let (reservations, server) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection: current,
                ..
            } if *current == connection => (*reservations, *server),
            _ => return false,
        };
        let reservation = reservations.map(|reservations| ActiveReservation { reservations });
        self.phase = SubscriptionPhase::Active {
            server,
            connection,
            reservation,
        };
        true
    }

    pub(crate) fn reset_for_server_termination(&mut self, server: ServerGeneration) -> bool {
        match &mut self.phase {
            SubscriptionPhase::Pending {
                server: current,
                commitment,
                ..
            } if *current == Some(server) => {
                *current = None;
                *commitment = PendingCommitment::Uncommitted;
                true
            }
            SubscriptionPhase::Connecting {
                reservations,
                server: current,
                commitment,
                ..
            } if *current == server => {
                let reservations = *reservations;
                *commitment = PendingCommitment::Uncommitted;
                self.phase = SubscriptionPhase::Pending {
                    reservations,
                    server: None,
                    commitment: PendingCommitment::Uncommitted,
                };
                true
            }
            _ => false,
        }
    }

    pub(crate) fn cleanup_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> (bool, bool) {
        match &mut self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server: current_server,
                connection: current_connection,
                ..
            } if *current_server == server && *current_connection == connection => {
                let reservations = *reservations;
                self.phase = SubscriptionPhase::Pending {
                    reservations,
                    server: None,
                    commitment: PendingCommitment::Uncommitted,
                };
                (true, self.can_remove())
            }
            SubscriptionPhase::Active {
                server: current_server,
                connection: current_connection,
                reservation,
            } if *current_server == server && *current_connection == connection => {
                let Some(active) = reservation.as_ref() else {
                    return (true, true);
                };
                let reservations = active.reservations;
                self.phase = SubscriptionPhase::Pending {
                    reservations: Some(reservations),
                    server: None,
                    commitment: PendingCommitment::Uncommitted,
                };
                (true, false)
            }
            _ => (false, false),
        }
    }
}

pub(crate) struct SubscriptionCatalog {
    pub(crate) entries: FxHashMap<SubscriptionId, SubscriptionEntry>,
    pub(crate) pending_topic_bytes: usize,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
}

struct PendingInsertPlan {
    id: SubscriptionId,
    key: SubscriptionKey,
    next_subscription_id: u64,
    identity: SubscriptionIdentity,
    pending_topic_bytes: usize,
}

impl SubscriptionCatalog {
    fn plan_pending_insert(
        &self,
        runtime_id: u64,
        source_id: SourceHandleId,
        topic: &RtdTopic,
        limits: RtdLimits,
    ) -> XllResult<PendingInsertPlan> {
        if self.pending_len() >= limits.max_pending.get() {
            return Err(XllError::Overloaded);
        }

        let pending_topic_bytes = self
            .pending_topic_bytes
            .checked_add(topic.byte_len())
            .filter(|&total| total <= limits.max_total_topic_bytes.get())
            .ok_or(XllError::Overloaded)?;

        let raw_id = self.next_subscription_id;
        let next_subscription_id = raw_id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
        })?;
        let id = SubscriptionId(raw_id);
        let key = SubscriptionKey::from_internal(runtime_id, id);
        let identity = SubscriptionIdentity {
            source_id: SourceId(source_id),
            topic: topic.clone(),
        };

        Ok(PendingInsertPlan {
            id,
            key,
            next_subscription_id,
            identity,
            pending_topic_bytes,
        })
    }

    fn commit_pending_insert(
        &mut self,
        plan: PendingInsertPlan,
        topic: RtdTopic,
    ) -> (SubscriptionId, SubscriptionKey) {
        let PendingInsertPlan {
            id,
            key,
            next_subscription_id,
            identity,
            pending_topic_bytes,
        } = plan;

        self.next_subscription_id = next_subscription_id;

        let previous = self.entries.insert(
            id,
            SubscriptionEntry {
                source_id: identity.source_id,
                topic,
                phase: SubscriptionPhase::Pending {
                    reservations: Some(NonZeroUsize::new(1).expect("one is non-zero")),
                    server: None,
                    commitment: PendingCommitment::Uncommitted,
                },
            },
        );
        if previous.is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.pending_topic_bytes = pending_topic_bytes;
        (id, key)
    }

    pub(crate) fn insert_pending(
        &mut self,
        runtime_id: u64,
        source_id: SourceHandleId,
        topic: RtdTopic,
        limits: RtdLimits,
    ) -> XllResult<(SubscriptionId, SubscriptionKey)> {
        let plan = self.plan_pending_insert(runtime_id, source_id, &topic, limits)?;
        self.identities
            .insert(plan.identity.clone(), plan.id, limits.max_source_ids.get())?;
        Ok(self.commit_pending_insert(plan, topic))
    }

    pub(crate) fn pending_len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.tracks_pending_bytes())
            .count()
    }

    pub(crate) fn with_entry<R>(
        &mut self,
        id: SubscriptionId,
        update: impl FnOnce(&mut SubscriptionEntry) -> R,
    ) -> Option<R> {
        let (was_pending, is_pending, topic_bytes, result) = {
            let entry = self.entries.get_mut(&id)?;
            let was_pending = entry.tracks_pending_bytes();
            let topic_bytes = entry.topic.byte_len();
            let result = update(entry);
            (
                was_pending,
                entry.tracks_pending_bytes(),
                topic_bytes,
                result,
            )
        };

        match (was_pending, is_pending) {
            (false, true) => {
                self.pending_topic_bytes = self
                    .pending_topic_bytes
                    .checked_add(topic_bytes)
                    .expect("pending topic byte accounting overflow");
            }
            (true, false) => {
                self.pending_topic_bytes =
                    checked_sub_or_abort(self.pending_topic_bytes, topic_bytes);
            }
            _ => {}
        }

        Some(result)
    }

    pub(crate) fn remove_entry(&mut self, id: SubscriptionId) -> Option<SubscriptionEntry> {
        let removed = self.entries.remove(&id)?;
        if removed.tracks_pending_bytes() {
            self.pending_topic_bytes =
                checked_sub_or_abort(self.pending_topic_bytes, removed.topic.byte_len());
        }
        let identity = SubscriptionIdentity {
            source_id: removed.source_id,
            topic: removed.topic.clone(),
        };
        if self.identities.remove(&identity) != Some(id) {
            xlfn_kernel::invariant::fail_stop();
        }
        Some(removed)
    }

    #[cfg(test)]
    pub(crate) fn assert_identity_invariants(&self) {
        self.identities.assert_invariants();

        assert_eq!(self.identities.id_by_identity.len(), self.entries.len());

        for (identity, id) in &self.identities.id_by_identity {
            let entry = self
                .entries
                .get(id)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());

            assert_eq!(entry.source_id, identity.source_id);
            assert_eq!(&entry.topic, &identity.topic);
        }

        let expected_pending_bytes = self
            .entries
            .values()
            .filter(|entry| entry.tracks_pending_bytes())
            .map(|entry| entry.topic.byte_len())
            .sum::<usize>();
        assert_eq!(self.pending_topic_bytes, expected_pending_bytes);

        for entry in self.entries.values() {
            match &entry.phase {
                SubscriptionPhase::Pending {
                    reservations,
                    server: _,
                    commitment,
                } => {
                    assert!(entry.connection_generation().is_none());
                    if reservations.is_none() {
                        assert_eq!(*commitment, PendingCommitment::Committed);
                    }
                }
                SubscriptionPhase::Connecting {
                    reservations: _,
                    server: _,
                    connection: _,
                    commitment: _,
                } => {
                    assert!(entry.connection_generation().is_some());
                }
                SubscriptionPhase::Active {
                    connection: _,
                    server: _,
                    reservation,
                } => {
                    assert!(entry.connection_generation().is_some());
                    if let Some(reservation) = reservation {
                        assert!(reservation.reservations.get() > 0);
                    }
                }
            }
        }
    }
}
