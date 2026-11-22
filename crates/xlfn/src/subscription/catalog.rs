use super::identity::{SubscriptionIdentityIndex, map_index_error};
use super::source::SourceHandleId;
use super::topic::{
    BorrowedTopicParts, RtdLimits, RtdTopic, SourceId, SubscriptionId, SubscriptionIdentityKey,
    SubscriptionKey,
};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;

use xlfn_services::rtd::quota::{CatalogQuota, PendingCharge};

use xlfn_services::rtd::{EntryError, EntryState};

pub(super) type PrepareTicket = xlfn_services::rtd::PrepareTicket<SubscriptionId>;
pub(super) type ConnectionTicket =
    xlfn_services::rtd::ConnectionTicket<SubscriptionId, ServerGeneration, ConnectionGeneration>;
pub(crate) use xlfn_services::rtd::PreparationFinish;

fn map_entry_error(error: EntryError) -> XllError {
    let diagnostic_id = match error {
        EntryError::ActiveKeyDuplicate => crate::diagnostics::catalog::ACTIVE_KEY_DUPLICATE,
        EntryError::Closing => return XllError::Closing,
        EntryError::ReservationOverflow => crate::diagnostics::catalog::RESERVATION_OVERFLOW,
        EntryError::ServerGenerationMismatch => {
            crate::diagnostics::catalog::SERVER_GENERATION_MISMATCH
        }
        EntryError::ConnectionInFlight => crate::diagnostics::catalog::CONNECTION_INFLIGHT,
    };
    crate::error::construct::internal(diagnostic_id)
}

pub(crate) struct SubscriptionEntry {
    pub(crate) source_id: SourceId,
    pub(crate) topic: RtdTopic,
    state: EntryState<SubscriptionId, ServerGeneration, ConnectionGeneration>,
}

impl SubscriptionEntry {
    #[cfg(test)]
    pub(crate) fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state.is_active()
    }

    fn is_identifiable(&self) -> bool {
        self.state.is_identifiable()
    }

    pub(crate) fn counts_as_pending(&self) -> bool {
        self.state.counts_as_pending()
    }

    pub(crate) fn can_remove(&self) -> bool {
        self.state.can_remove()
    }

    pub(crate) fn server_generation(&self) -> Option<ServerGeneration> {
        self.state.server_generation()
    }

    pub(crate) fn connection_generation(&self) -> Option<ConnectionGeneration> {
        self.state.connection_generation()
    }

    pub(super) fn add_reservation(&mut self) -> XllResult<PrepareTicket> {
        self.state.add_reservation().map_err(map_entry_error)
    }

    pub(super) fn finish_preparation(
        &mut self,
        ticket: PrepareTicket,
        committed: bool,
    ) -> PreparationFinish {
        self.state.finish_preparation(ticket, committed)
    }

    pub(crate) fn claim_server(&mut self, generation: ServerGeneration) -> XllResult<()> {
        self.state.claim_server(generation).map_err(map_entry_error)
    }

    pub(super) fn begin_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> XllResult<ConnectionTicket> {
        self.state
            .begin_connection(server, connection)
            .map_err(map_entry_error)
    }

    pub(super) fn rollback_connection(&mut self, ticket: &ConnectionTicket) -> bool {
        self.state.rollback_connection(ticket)
    }

    pub(super) fn finish_connection(&mut self, ticket: &ConnectionTicket) -> bool {
        self.state.finish_connection(ticket)
    }

    pub(crate) fn revoke_for_server_termination(&mut self, server: ServerGeneration) -> bool {
        self.state.revoke_for_server_termination(server)
    }

    pub(crate) fn cleanup_connection(
        &mut self,
        server: ServerGeneration,
        connection: ConnectionGeneration,
    ) -> (bool, bool) {
        self.state.cleanup_connection(server, connection)
    }
}

pub(crate) struct SubscriptionCatalog {
    pub(crate) entries: FxHashMap<SubscriptionId, SubscriptionEntry>,
    pub(crate) quota: CatalogQuota,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
}

struct PendingInsertPlan<'catalog, 'parts, Part: AsRef<str>> {
    entry: std::collections::hash_map::VacantEntry<'catalog, SubscriptionId, SubscriptionEntry>,
    identity: xlfn_services::rtd::index::IdentityInsertion<
        'catalog,
        SubscriptionIdentityKey,
        SubscriptionId,
    >,
    topic: BorrowedTopicParts<'parts, Part>,
    runtime_id: u64,
    id: SubscriptionId,
    key: SubscriptionKey,
    source_id: SourceId,
    next_id: &'catalog mut u64,
    charge: PendingCharge<'catalog>,
    next_subscription_id: u64,
}
impl<Part: AsRef<str>> PendingInsertPlan<'_, '_, Part> {
    fn commit(self) -> (PrepareTicket, SubscriptionKey) {
        // User-supplied AsRef conversion may unwind. No map/accounting mutation
        // occurs until this owner has produced its exact canonical topic.
        let topic = self.topic.into_owned();
        let (state, ticket) = EntryState::new_pending(self.runtime_id, self.id);
        self.identity.commit();
        self.entry.insert(SubscriptionEntry {
            source_id: self.source_id,
            topic,
            state,
        });
        *self.next_id = self.next_subscription_id;
        self.charge.commit();
        (ticket, self.key)
    }
}

impl SubscriptionCatalog {
    pub(crate) fn clear(&mut self) {
        self.identities.clear();
        self.quota.clear();
        self.entries.clear();
    }

    pub(crate) fn find_identity<Part: AsRef<str>>(
        &self,
        source_id: SourceId,
        topic: &BorrowedTopicParts<'_, Part>,
    ) -> XllResult<Option<(SubscriptionId, &SubscriptionEntry)>> {
        let key = topic.identity_key(source_id);
        // Hashes only narrow the candidates; entries own the canonical topics.
        for &id in self.identities.candidates(key) {
            let entry = self
                .entries
                .get(&id)
                .ok_or(crate::error::construct::internal(
                    crate::diagnostics::catalog::RTD_INDEX_ORPHAN,
                ))?;
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
        let charge = self
            .quota
            .prepare_insert(
                topic.byte_len(),
                limits.max_pending.get(),
                limits.max_total_topic_bytes.get(),
            )
            .ok_or(XllError::Overloaded)?;
        let raw_id = self.next_subscription_id;
        let next_subscription_id =
            raw_id
                .checked_add(1)
                .ok_or(crate::error::construct::internal(
                    crate::diagnostics::catalog::RTD_SUBSCRIPTION_OVERFLOW,
                ))?;
        let id = SubscriptionId(raw_id);
        let key = SubscriptionKey::from_internal(runtime_id, id);
        let source_id = SourceId(source_id);
        let identity_key = topic.identity_key(source_id);
        let identity = self
            .identities
            .prepare_insert(identity_key, id, limits.max_source_ids.get())
            .map_err(map_index_error)?;
        let std::collections::hash_map::Entry::Vacant(entry) = self.entries.entry(id) else {
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::RTD_INDEX_DUPLICATE,
            ));
        };
        Ok(PendingInsertPlan {
            entry,
            identity,
            topic,
            runtime_id,
            id,
            key,
            source_id,
            next_id: &mut self.next_subscription_id,
            charge,
            next_subscription_id,
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
        self.quota.pending()
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
        self.quota
            .transition(was_pending, entry.counts_as_pending());

        Some(result)
    }

    pub(crate) fn remove_entry(&mut self, id: SubscriptionId) -> Option<SubscriptionEntry> {
        let removed = self.entries.remove(&id)?;
        self.quota
            .remove(removed.topic.byte_len(), removed.counts_as_pending());
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
        for (key, ids) in self.identities.buckets() {
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
        assert_eq!(self.quota.topic_bytes(), expected_retained_bytes);
        assert_eq!(
            self.quota.pending(),
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
