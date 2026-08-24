use super::identity::{SourceIdentityRegistry, SubscriptionIdentityIndex};
use super::source::ErasedRtdSource;
use super::topic::{RtdTopic, SubscriptionKey};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubscriptionState {
    Pending,
    Connecting,
    Active,
}

pub(crate) struct SubscriptionEntry {
    pub(crate) source: Option<Arc<dyn ErasedRtdSource>>,
    pub(crate) topic: RtdTopic,
    pub(crate) state: SubscriptionState,
    pub(crate) live_reservations: usize,
    pub(crate) committed: bool,
    pub(crate) server_generation: Option<ServerGeneration>,
    pub(crate) connection_generation: Option<ConnectionGeneration>,
}

impl SubscriptionEntry {
    pub(crate) fn is_connected(&self) -> bool {
        matches!(
            self.state,
            SubscriptionState::Connecting | SubscriptionState::Active
        )
    }

    pub(crate) fn tracks_pending_bytes(&self) -> bool {
        self.state != SubscriptionState::Active || self.live_reservations != 0
    }

    pub(crate) fn can_remove(&self) -> bool {
        self.state == SubscriptionState::Pending
            && self.connection_generation.is_none()
            && !self.committed
            && self.live_reservations == 0
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
            match entry.state {
                SubscriptionState::Pending => {
                    assert!(entry.source.is_some());
                    assert!(entry.connection_generation.is_none());
                }
                SubscriptionState::Connecting => {
                    assert!(entry.source.is_some());
                    assert!(entry.connection_generation.is_some());
                }
                SubscriptionState::Active => {
                    assert!(entry.connection_generation.is_some());
                    assert_eq!(entry.source.is_some(), entry.live_reservations != 0);
                    assert!(entry.committed);
                }
            }
        }
    }
}
