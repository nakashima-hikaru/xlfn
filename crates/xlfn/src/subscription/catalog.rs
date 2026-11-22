use super::identity::{SubscriptionIdentityIndex, map_index_error};
use super::source::SourceHandleId;
use super::topic::{
    BorrowedTopicParts, RtdLimits, RtdTopic, SourceId, SubscriptionId, SubscriptionIdentityKey,
    SubscriptionKey,
};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;

use xlfn_services::rtd::quota::CatalogQuota;
use xlfn_services::rtd::{EntryError, EntryState};

pub(super) type PrepareTicket = xlfn_services::rtd::PrepareTicket<SubscriptionId>;
pub(super) type ConnectionTicket =
    xlfn_services::rtd::ConnectionTicket<SubscriptionId, ServerGeneration, ConnectionGeneration>;
pub(crate) type SubscriptionEntryState =
    EntryState<SubscriptionId, ServerGeneration, ConnectionGeneration>;
pub(crate) use xlfn_services::rtd::PreparationFinish;

pub(crate) fn map_entry_error(error: EntryError) -> XllError {
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
    state: SubscriptionEntryState,
}

impl SubscriptionEntry {
    pub(crate) fn state(&self) -> &SubscriptionEntryState {
        &self.state
    }
}

pub(crate) struct SubscriptionCatalog {
    pub(crate) entries: FxHashMap<SubscriptionId, SubscriptionEntry>,
    pub(crate) quota: CatalogQuota,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
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

    pub(super) fn insert_pending<Part: AsRef<str>>(
        &mut self,
        runtime_id: u64,
        source_id: SourceHandleId,
        topic: BorrowedTopicParts<'_, Part>,
        limits: RtdLimits,
    ) -> XllResult<(PrepareTicket, SubscriptionKey)> {
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
        // User-supplied AsRef conversion may unwind. No map/accounting mutation
        // occurs until this owner has produced its exact canonical topic.
        let topic = topic.into_owned();
        let (state, ticket) = EntryState::new_pending(runtime_id, id);
        identity.commit();
        entry.insert(SubscriptionEntry {
            source_id,
            topic,
            state,
        });
        self.next_subscription_id = next_subscription_id;
        charge.commit();
        Ok((ticket, key))
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.quota.pending()
    }

    pub(crate) fn with_entry<R>(
        &mut self,
        id: SubscriptionId,
        update: impl FnOnce(&mut SubscriptionEntryState) -> R,
    ) -> Option<R> {
        let entry = self.entries.get_mut(&id)?;
        let was_pending = entry.state.counts_as_pending();
        let was_identifiable = entry.state.is_identifiable();
        let result = update(&mut entry.state);
        if was_identifiable && !entry.state.is_identifiable() {
            let identity_key = SubscriptionIdentityKey::new(entry.source_id, &entry.topic);
            if !self.identities.remove(identity_key, id) {
                xlfn_kernel::invariant::fail_stop();
            }
        }
        self.quota
            .transition(was_pending, entry.state.counts_as_pending());

        Some(result)
    }

    pub(crate) fn remove_entry(&mut self, id: SubscriptionId) -> Option<SubscriptionEntry> {
        let removed = self.entries.remove(&id)?;
        self.quota
            .remove(removed.topic.byte_len(), removed.state.counts_as_pending());
        let identity_key = SubscriptionIdentityKey::new(removed.source_id, &removed.topic);
        if removed.state.is_identifiable() && !self.identities.remove(identity_key, id) {
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
                assert!(entry.state.is_identifiable());
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
                .filter(|entry| entry.state.is_identifiable())
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
                .filter(|entry| entry.state.counts_as_pending())
                .count()
        );

        for entry in self.entries.values() {
            assert!(
                !entry.state.can_remove(),
                "remove terminal entries under the catalog lock"
            );
        }
    }
}
