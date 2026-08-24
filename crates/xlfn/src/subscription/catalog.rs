use super::identity::{SourceIdentityRegistry, SubscriptionIdentityIndex};
use super::source::ErasedRtdSource;
use super::topic::{RtdTopic, SubscriptionKey};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingCommitment {
    Uncommitted,
    Committed,
}

pub(crate) struct ActiveReservation {
    source: Arc<dyn ErasedRtdSource>,
    reservations: NonZeroUsize,
}

pub(crate) enum SubscriptionPhase {
    Pending {
        source: Arc<dyn ErasedRtdSource>,
        reservations: Option<NonZeroUsize>,
        server: Option<ServerGeneration>,
        commitment: PendingCommitment,
    },
    Connecting {
        source: Arc<dyn ErasedRtdSource>,
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
    DropSource(Arc<dyn ErasedRtdSource>),
}

pub(crate) struct SubscriptionEntry {
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

    pub(crate) fn into_source(self) -> Option<Arc<dyn ErasedRtdSource>> {
        match self.phase {
            SubscriptionPhase::Pending { source, .. }
            | SubscriptionPhase::Connecting { source, .. } => Some(source),
            SubscriptionPhase::Active { reservation, .. } => {
                reservation.map(|reservation| reservation.source)
            }
        }
    }

    pub(crate) fn add_reservation(&mut self) -> XllResult<()> {
        let reservations = match &mut self.phase {
            SubscriptionPhase::Pending { reservations, .. }
            | SubscriptionPhase::Connecting { reservations, .. } => reservations,
            SubscriptionPhase::Active { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }
        };
        let next = reservations
            .map_or(0, NonZeroUsize::get)
            .checked_add(1)
            .ok_or(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::RESERVATION_OVERFLOW,
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
                    let source = Arc::clone(&active.source);
                    *reservation = None;
                    PreparationFinish::DropSource(source)
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
                        diagnostic_id: crate::error::DiagnosticId::SERVER_GENERATION_MISMATCH,
                    });
                }
                return Ok(());
            }
        };
        if let Some(existing) = *server {
            if existing != generation {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::SERVER_GENERATION_MISMATCH,
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
    ) -> XllResult<Arc<dyn ErasedRtdSource>> {
        let (source, reservations, commitment, existing_server) = match &self.phase {
            SubscriptionPhase::Pending {
                source,
                reservations,
                commitment,
                server,
            } => (source, *reservations, *commitment, *server),
            SubscriptionPhase::Connecting { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::CONNECTION_INFLIGHT,
                });
            }
            SubscriptionPhase::Active { .. } => {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }
        };
        if let Some(existing) = existing_server
            && existing != server
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::SERVER_GENERATION_MISMATCH,
            });
        }
        let source = Arc::clone(source);
        self.phase = SubscriptionPhase::Connecting {
            source: Arc::clone(&source),
            reservations,
            server,
            connection,
            commitment,
        };
        Ok(source)
    }

    pub(crate) fn rollback_connection(&mut self, connection: ConnectionGeneration) -> bool {
        let (source, reservations, server, commitment) = match &self.phase {
            SubscriptionPhase::Connecting {
                source,
                reservations,
                server,
                connection: current,
                commitment,
            } if *current == connection => (source, *reservations, *server, *commitment),
            _ => return false,
        };
        self.phase = SubscriptionPhase::Pending {
            source: Arc::clone(source),
            reservations,
            server: Some(server),
            commitment,
        };
        true
    }

    pub(crate) fn finish_connection(
        &mut self,
        connection: ConnectionGeneration,
    ) -> Option<Arc<dyn ErasedRtdSource>> {
        let (source, reservations, server) = match &self.phase {
            SubscriptionPhase::Connecting {
                source,
                reservations,
                server,
                connection: current,
                ..
            } if *current == connection => (source, *reservations, *server),
            _ => return None,
        };
        let drop_source = reservations.is_none().then(|| Arc::clone(source));
        let reservation = reservations.map(|reservations| ActiveReservation {
            source: Arc::clone(source),
            reservations,
        });
        self.phase = SubscriptionPhase::Active {
            server,
            connection,
            reservation,
        };
        drop_source
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
                source,
                reservations,
                server: current,
                commitment,
                ..
            } if *current == server => {
                let source = Arc::clone(source);
                let reservations = *reservations;
                *commitment = PendingCommitment::Uncommitted;
                self.phase = SubscriptionPhase::Pending {
                    source,
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
                source,
                reservations,
                server: current_server,
                connection: current_connection,
                ..
            } if *current_server == server && *current_connection == connection => {
                let source = Arc::clone(source);
                let reservations = *reservations;
                self.phase = SubscriptionPhase::Pending {
                    source,
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
                let source = Arc::clone(&active.source);
                let reservations = active.reservations;
                self.phase = SubscriptionPhase::Pending {
                    source,
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
    pub(crate) entries: FxHashMap<SubscriptionKey, SubscriptionEntry>,
    pub(crate) pending_topic_bytes: usize,
    pub(crate) sources: SourceIdentityRegistry,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
}

impl SubscriptionCatalog {
    pub(crate) fn allocate_key(&mut self, runtime_id: u64) -> XllResult<SubscriptionKey> {
        let id = self.next_subscription_id;
        self.next_subscription_id = id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
        })?;
        Ok(SubscriptionKey::from_allocated_id(runtime_id, id))
    }

    pub(crate) fn pending_len(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.tracks_pending_bytes())
            .count()
    }

    pub(crate) fn with_entry<R>(
        &mut self,
        key: &SubscriptionKey,
        update: impl FnOnce(&mut SubscriptionEntry) -> R,
    ) -> Option<R> {
        let (was_pending, is_pending, topic_bytes, result) = {
            let entry = self.entries.get_mut(key)?;
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
                self.pending_topic_bytes = self.pending_topic_bytes.saturating_sub(topic_bytes);
            }
            _ => {}
        }

        Some(result)
    }

    pub(crate) fn remove_entry(&mut self, key: &SubscriptionKey) -> Option<SubscriptionEntry> {
        let removed = self.entries.remove(key)?;
        if removed.tracks_pending_bytes() {
            self.pending_topic_bytes = self
                .pending_topic_bytes
                .saturating_sub(removed.topic.byte_len());
        }
        if let Some(identity) = self.identities.remove_by_key(key) {
            self.sources.release_source(identity.source_id.0);
        }
        Some(removed)
    }

    #[cfg(test)]
    pub(crate) fn assert_identity_invariants(&self) {
        assert_eq!(
            self.identities.key_by_identity.len(),
            self.identities.identity_by_key.len(),
        );

        for (identity, key) in &self.identities.key_by_identity {
            assert_eq!(self.identities.identity_by_key.get(key), Some(identity),);

            assert!(self.entries.contains_key(key));
        }

        let mut expected_source_refs = FxHashMap::default();
        for identity in self.identities.key_by_identity.keys() {
            *expected_source_refs
                .entry(identity.source_id.0)
                .or_insert(0) += 1;
        }
        assert_eq!(expected_source_refs.len(), self.sources.refs.len());
        for (source_id, refs) in expected_source_refs {
            assert_eq!(
                self.sources.refs.get(&source_id).map(|value| value.get()),
                Some(refs),
            );
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
                    source,
                    reservations,
                    server: _,
                    commitment,
                } => {
                    assert!(Arc::strong_count(source) >= 1);
                    assert!(entry.connection_generation().is_none());
                    if reservations.is_none() {
                        assert_eq!(*commitment, PendingCommitment::Committed);
                    }
                }
                SubscriptionPhase::Connecting {
                    source,
                    reservations: _,
                    server: _,
                    connection: _,
                    commitment: _,
                } => {
                    assert!(Arc::strong_count(source) >= 1);
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
                        assert!(Arc::strong_count(&reservation.source) >= 1);
                    }
                }
            }
        }
    }
}
