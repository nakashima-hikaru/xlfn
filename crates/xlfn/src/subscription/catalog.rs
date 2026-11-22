use super::identity::SubscriptionIdentityIndex;
use super::source::SourceHandleId;
use super::topic::{
    BorrowedTopicParts, RtdLimits, RtdTopic, SourceId, SubscriptionId, SubscriptionIdentityKey,
    SubscriptionKey,
};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;

use xlfn_kernel::invariant::checked_sub_or_abort;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingCommitment {
    Uncommitted,
    Committed,
}

/// A transport key's identity is never reused, even when the same formula
/// starts another preparation after its old connection was revoked.
#[derive(Clone, Copy, PartialEq, Eq)]
struct EntryIncarnation {
    runtime: u64,
    id: SubscriptionId,
}

pub(super) struct PrepareTicket(EntryIncarnation);

impl PrepareTicket {
    pub(super) fn id(&self) -> SubscriptionId {
        self.0.id
    }
}

enum SubscriptionPhase {
    Pending {
        reservations: Option<NonZeroUsize>,
        server: Option<ServerGeneration>,
    },
    AwaitConnect {
        reservations: Option<NonZeroUsize>,
        server: Option<ServerGeneration>,
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
        reservations: Option<NonZeroUsize>,
    },
    /// Unindexed and unable to connect. Until all old preparations return,
    /// this entry still owns its pending and topic-byte quota.
    Revoked { reservations: Option<NonZeroUsize> },
}

/// This affine ticket is issued by the entry which begins the connection;
/// only its exact incarnation and server/connection generations may use it.
pub(super) struct ConnectionTicket {
    entry: EntryIncarnation,
    server: ServerGeneration,
    connection: ConnectionGeneration,
}

impl ConnectionTicket {
    pub(super) fn generation(&self) -> ConnectionGeneration {
        self.connection
    }
}

pub(crate) enum PreparationFinish {
    Keep,
    Remove,
}

pub(crate) struct SubscriptionEntry {
    pub(crate) source_id: SourceId,
    pub(crate) topic: RtdTopic,
    phase: SubscriptionPhase,
    incarnation: EntryIncarnation,
}

impl SubscriptionEntry {
    #[cfg(test)]
    pub(crate) fn is_connected(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Connecting { .. } | SubscriptionPhase::Active { .. }
        )
    }

    pub(crate) fn is_active(&self) -> bool {
        matches!(self.phase, SubscriptionPhase::Active { .. })
    }

    fn is_identifiable(&self) -> bool {
        !matches!(self.phase, SubscriptionPhase::Revoked { .. })
    }

    pub(crate) fn counts_as_pending(&self) -> bool {
        !matches!(
            self.phase,
            SubscriptionPhase::Active {
                reservations: None,
                ..
            }
        )
    }

    pub(crate) fn can_remove(&self) -> bool {
        matches!(
            self.phase,
            SubscriptionPhase::Pending {
                reservations: None,
                ..
            } | SubscriptionPhase::Revoked { reservations: None }
        )
    }

    pub(crate) fn server_generation(&self) -> Option<ServerGeneration> {
        match &self.phase {
            SubscriptionPhase::Pending { server, .. }
            | SubscriptionPhase::AwaitConnect { server, .. } => *server,
            SubscriptionPhase::Connecting { server, .. }
            | SubscriptionPhase::Active { server, .. } => Some(*server),
            SubscriptionPhase::Revoked { .. } => None,
        }
    }

    pub(crate) fn connection_generation(&self) -> Option<ConnectionGeneration> {
        match &self.phase {
            SubscriptionPhase::Pending { .. }
            | SubscriptionPhase::AwaitConnect { .. }
            | SubscriptionPhase::Revoked { .. } => None,
            SubscriptionPhase::Connecting { connection, .. }
            | SubscriptionPhase::Active { connection, .. } => Some(*connection),
        }
    }

    pub(super) fn add_reservation(&mut self) -> XllResult<PrepareTicket> {
        let reservations = match &mut self.phase {
            SubscriptionPhase::Pending { reservations, .. }
            | SubscriptionPhase::AwaitConnect { reservations, .. }
            | SubscriptionPhase::Connecting { reservations, .. } => reservations,
            SubscriptionPhase::Active { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }
            SubscriptionPhase::Revoked { .. } => return Err(XllError::Closing),
        };
        *reservations = Some(
            reservations
                .map_or(Some(NonZeroUsize::MIN), |count| count.checked_add(1))
                .ok_or(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RESERVATION_OVERFLOW,
                })?,
        );
        Ok(PrepareTicket(self.incarnation))
    }

    pub(super) fn finish_preparation(
        &mut self,
        ticket: PrepareTicket,
        committed: bool,
    ) -> PreparationFinish {
        if ticket.0 != self.incarnation {
            return PreparationFinish::Keep;
        }
        let reservations = match &mut self.phase {
            SubscriptionPhase::Pending { reservations, .. }
            | SubscriptionPhase::AwaitConnect { reservations, .. }
            | SubscriptionPhase::Connecting { reservations, .. }
            | SubscriptionPhase::Active { reservations, .. }
            | SubscriptionPhase::Revoked { reservations } => reservations,
        };
        let Some(count) = *reservations else {
            xlfn_kernel::invariant::fail_stop();
        };
        *reservations = NonZeroUsize::new(count.get() - 1);
        if committed {
            match &mut self.phase {
                SubscriptionPhase::Pending {
                    reservations,
                    server,
                } => {
                    self.phase = SubscriptionPhase::AwaitConnect {
                        reservations: *reservations,
                        server: *server,
                    };
                }
                SubscriptionPhase::Connecting { commitment, .. } => {
                    *commitment = PendingCommitment::Committed;
                }
                SubscriptionPhase::AwaitConnect { .. }
                | SubscriptionPhase::Active { .. }
                | SubscriptionPhase::Revoked { .. } => {}
            }
        }
        if self.can_remove() {
            PreparationFinish::Remove
        } else {
            PreparationFinish::Keep
        }
    }

    pub(crate) fn claim_server(&mut self, generation: ServerGeneration) -> XllResult<()> {
        let server = match &mut self.phase {
            SubscriptionPhase::Pending { server, .. }
            | SubscriptionPhase::AwaitConnect { server, .. } => server,
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
            SubscriptionPhase::Revoked { .. } => return Err(XllError::Closing),
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

    pub(super) fn begin_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> XllResult<ConnectionTicket> {
        let (reservations, commitment, existing_server) = match &self.phase {
            SubscriptionPhase::Pending {
                reservations,
                server,
            } => (*reservations, PendingCommitment::Uncommitted, *server),
            SubscriptionPhase::AwaitConnect {
                reservations,
                server,
            } => (*reservations, PendingCommitment::Committed, *server),
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
            SubscriptionPhase::Revoked { .. } => return Err(XllError::Closing),
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
        Ok(ConnectionTicket {
            entry: self.incarnation,
            server,
            connection,
        })
    }

    pub(super) fn rollback_connection(&mut self, ticket: &ConnectionTicket) -> bool {
        let (reservations, server, commitment) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection,
                commitment,
            } if ticket.entry == self.incarnation
                && *connection == ticket.connection
                && *server == ticket.server =>
            {
                (*reservations, Some(*server), *commitment)
            }
            _ => return false,
        };
        self.phase = match commitment {
            PendingCommitment::Uncommitted => SubscriptionPhase::Pending {
                reservations,
                server,
            },
            PendingCommitment::Committed => SubscriptionPhase::AwaitConnect {
                reservations,
                server,
            },
        };
        true
    }

    pub(super) fn finish_connection(&mut self, ticket: &ConnectionTicket) -> bool {
        let (reservations, server) = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server,
                connection,
                ..
            } if ticket.entry == self.incarnation
                && *connection == ticket.connection
                && *server == ticket.server =>
            {
                (*reservations, *server)
            }
            _ => return false,
        };
        self.phase = SubscriptionPhase::Active {
            server,
            connection: ticket.connection,
            reservations,
        };
        true
    }

    pub(crate) fn revoke_for_server_termination(&mut self, server: ServerGeneration) -> bool {
        let reservations = match &self.phase {
            SubscriptionPhase::Pending {
                server: current,
                reservations,
            }
            | SubscriptionPhase::AwaitConnect {
                server: current,
                reservations,
            } if *current == Some(server) => *reservations,
            SubscriptionPhase::Connecting {
                server: current,
                reservations,
                ..
            } if *current == server => *reservations,
            _ => return false,
        };
        self.phase = SubscriptionPhase::Revoked { reservations };
        true
    }

    pub(crate) fn cleanup_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> (bool, bool) {
        let reservations = match &self.phase {
            SubscriptionPhase::Connecting {
                reservations,
                server: current_server,
                connection: current_connection,
                ..
            }
            | SubscriptionPhase::Active {
                reservations,
                server: current_server,
                connection: current_connection,
            } if *current_server == server && *current_connection == connection => *reservations,
            _ => return (false, false),
        };
        self.phase = SubscriptionPhase::Revoked { reservations };
        (true, self.can_remove())
    }
}

pub(crate) struct SubscriptionCatalog {
    pub(crate) entries: FxHashMap<SubscriptionId, SubscriptionEntry>,
    pub(crate) retained_topic_bytes: usize,
    pub(crate) pending_count: usize,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
}

struct PendingInsertPlan<'catalog, 'parts, Part: AsRef<str>> {
    entry: std::collections::hash_map::VacantEntry<'catalog, SubscriptionId, SubscriptionEntry>,
    identity: super::identity::IdentityInsertion<'catalog>,
    topic: BorrowedTopicParts<'parts, Part>,
    incarnation: EntryIncarnation,
    key: SubscriptionKey,
    source_id: SourceId,
    next_id: &'catalog mut u64,
    retained_bytes: &'catalog mut usize,
    pending_count: &'catalog mut usize,
    next_subscription_id: u64,
    retained_topic_bytes: usize,
}
impl<Part: AsRef<str>> PendingInsertPlan<'_, '_, Part> {
    fn commit(self) -> (PrepareTicket, SubscriptionKey) {
        // User-supplied AsRef conversion may unwind. No map/accounting mutation
        // occurs until this owner has produced its exact canonical topic.
        let topic = self.topic.into_owned();
        self.identity.commit();
        self.entry.insert(SubscriptionEntry {
            source_id: self.source_id,
            topic,
            incarnation: self.incarnation,
            phase: SubscriptionPhase::Pending {
                reservations: Some(NonZeroUsize::MIN),
                server: None,
            },
        });
        *self.next_id = self.next_subscription_id;
        *self.retained_bytes = self.retained_topic_bytes;
        *self.pending_count += 1;
        (PrepareTicket(self.incarnation), self.key)
    }
}

impl SubscriptionCatalog {
    pub(crate) fn find_identity<Part: AsRef<str>>(
        &self,
        source_id: SourceId,
        topic: &BorrowedTopicParts<'_, Part>,
    ) -> XllResult<Option<(SubscriptionId, &SubscriptionEntry)>> {
        let key = topic.identity_key(source_id);
        // Hashes only narrow the candidates; entries own the canonical topics.
        for &id in self.identities.candidates(key) {
            let entry = self.entries.get(&id).ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_ORPHAN,
            })?;
            if entry.source_id == source_id && topic.matches(&entry.topic) {
                return Ok(Some((id, entry)));
            }
        }
        Ok(None)
    }

    fn plan_pending_insert<'catalog, 'parts, Part: AsRef<str>>(
        &'catalog mut self,
        runtime_id: u64,
        source_id: SourceHandleId,
        topic: BorrowedTopicParts<'parts, Part>,
        limits: RtdLimits,
    ) -> XllResult<PendingInsertPlan<'catalog, 'parts, Part>> {
        if self.pending_count >= limits.max_pending.get() {
            return Err(XllError::Overloaded);
        }
        let retained_topic_bytes = self
            .retained_topic_bytes
            .checked_add(topic.byte_len())
            .filter(|&total| total <= limits.max_total_topic_bytes.get())
            .ok_or(XllError::Overloaded)?;
        let raw_id = self.next_subscription_id;
        let next_subscription_id = raw_id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
        })?;
        let id = SubscriptionId(raw_id);
        let key = SubscriptionKey::from_internal(runtime_id, id);
        let source_id = SourceId(source_id);
        let identity_key = topic.identity_key(source_id);
        let identity =
            self.identities
                .prepare_insert(identity_key, id, limits.max_source_ids.get())?;
        let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(id) else {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_DUPLICATE,
            });
        };
        Ok(PendingInsertPlan {
            entry,
            identity,
            topic,
            incarnation: EntryIncarnation {
                runtime: runtime_id,
                id,
            },
            key,
            source_id,
            next_id: &mut self.next_subscription_id,
            retained_bytes: &mut self.retained_topic_bytes,
            pending_count: &mut self.pending_count,
            next_subscription_id,
            retained_topic_bytes,
        })
    }

    pub(super) fn insert_pending<Part: AsRef<str>>(
        &mut self,
        runtime_id: u64,
        source_id: SourceHandleId,
        topic: BorrowedTopicParts<'_, Part>,
        limits: RtdLimits,
    ) -> XllResult<(PrepareTicket, SubscriptionKey)> {
        Ok(self
            .plan_pending_insert(runtime_id, source_id, topic, limits)?
            .commit())
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending_count
    }

    pub(crate) fn with_entry<R>(
        &mut self,
        id: SubscriptionId,
        update: impl FnOnce(&mut SubscriptionEntry) -> R,
    ) -> Option<R> {
        let entry = self.entries.get_mut(&id)?;
        let was_pending = entry.counts_as_pending();
        let was_identifiable = entry.is_identifiable();
        let result = update(entry);
        if was_identifiable && !entry.is_identifiable() {
            let identity_key = SubscriptionIdentityKey::new(entry.source_id, &entry.topic);
            if !self.identities.remove(identity_key, id) {
                xlfn_kernel::invariant::fail_stop();
            }
        }
        match (was_pending, entry.counts_as_pending()) {
            (false, true) => self.pending_count += 1,
            (true, false) => self.pending_count = checked_sub_or_abort(self.pending_count, 1),
            _ => {}
        }

        Some(result)
    }

    pub(crate) fn remove_entry(&mut self, id: SubscriptionId) -> Option<SubscriptionEntry> {
        let removed = self.entries.remove(&id)?;
        if removed.counts_as_pending() {
            self.pending_count = checked_sub_or_abort(self.pending_count, 1);
        }
        self.retained_topic_bytes =
            checked_sub_or_abort(self.retained_topic_bytes, removed.topic.byte_len());
        let identity_key = SubscriptionIdentityKey::new(removed.source_id, &removed.topic);
        if removed.is_identifiable() && !self.identities.remove(identity_key, id) {
            xlfn_kernel::invariant::fail_stop();
        }
        Some(removed)
    }

    #[cfg(test)]
    pub(crate) fn assert_identity_invariants(&self) {
        self.identities.assert_invariants();

        let mut seen_ids = rustc_hash::FxHashSet::default();
        for (key, ids) in &self.identities.ids_by_key {
            assert!(!ids.is_empty());
            for (position, id) in ids.iter().enumerate() {
                assert!(
                    seen_ids.insert(*id),
                    "each subscription is indexed exactly once"
                );
                let entry = self.entries.get(id).expect("indexed entry exists");
                assert!(entry.is_identifiable());
                assert_eq!(entry.source_id, key.source_id);
                assert_eq!(entry.topic.identity_hash(), key.topic_hash);
                for other_id in &ids[..position] {
                    assert_ne!(entry.topic, self.entries[other_id].topic);
                }
            }
        }
        assert_eq!(
            seen_ids.len(),
            self.entries
                .values()
                .filter(|entry| entry.is_identifiable())
                .count()
        );

        let expected_retained_bytes = self
            .entries
            .values()
            .map(|entry| entry.topic.byte_len())
            .sum::<usize>();
        assert_eq!(self.retained_topic_bytes, expected_retained_bytes);
        assert_eq!(
            self.pending_count,
            self.entries
                .values()
                .filter(|entry| entry.counts_as_pending())
                .count()
        );

        for entry in self.entries.values() {
            assert!(
                !entry.can_remove(),
                "remove terminal entries under the catalog lock"
            );
        }
    }
}

#[cfg(all(test, feature = "rtd"))]
mod typestate_tests {
    use super::*;
    use crate::generation::RuntimeGeneration;

    #[test]
    fn connection_ticket_cannot_finish_or_rollback_a_new_server_attempt() {
        let server = ServerGeneration::new(1).unwrap();
        let next_server = ServerGeneration::new(2).unwrap();
        // Reusing a connection generation here deliberately isolates the server
        // identity check in addition to the normal connection-generation check.
        let connection = ConnectionGeneration::new(1).unwrap();
        let mut entry = SubscriptionEntry {
            source_id: SourceId(SourceHandleId {
                generation: RuntimeGeneration::new(1).unwrap(),
                registration: 1,
                sequence: 1,
            }),
            topic: RtdTopic::single("ticket-generation").unwrap(),
            incarnation: EntryIncarnation {
                runtime: 1,
                id: SubscriptionId(1),
            },
            phase: SubscriptionPhase::Pending {
                reservations: NonZeroUsize::new(1),
                server: None,
            },
        };
        let old = entry.begin_connection(server, connection).unwrap();
        assert!(entry.revoke_for_server_termination(server));
        assert!(entry.begin_connection(next_server, connection).is_err());
        let mut replacement = SubscriptionEntry {
            source_id: entry.source_id,
            topic: entry.topic.clone(),
            incarnation: EntryIncarnation {
                runtime: 1,
                id: SubscriptionId(2),
            },
            phase: SubscriptionPhase::AwaitConnect {
                reservations: None,
                server: None,
            },
        };
        // Same server/connection numbers still cannot authorize a new entry.
        let current = replacement.begin_connection(server, connection).unwrap();
        assert!(!replacement.finish_connection(&old));
        assert!(!replacement.rollback_connection(&old));
        assert!(!replacement.is_active());
        assert_eq!(replacement.server_generation(), Some(server));
        assert!(replacement.finish_connection(&current));
        assert!(replacement.is_active());
    }
}
