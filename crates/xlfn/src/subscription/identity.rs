use super::source::SourceHandleId;
use super::topic::{SubscriptionIdentity, SubscriptionKey};
use crate::{XllError, XllResult};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ResolvedSourceIdentity {
    pub(crate) source_id: SourceHandleId,
    pub(crate) newly_registered: bool,
}

// Source handles carry a generation-owned identity. The registry therefore
// stores only that typed identity; it never derives identity from an allocation
// address and does not need an ownership anchor. Registered handle identities
// remain reserved until the subscription runtime is cleared.
pub(crate) struct SourceIdentityRegistry {
    pub(crate) ids: FxHashSet<SourceHandleId>,
}

impl SourceIdentityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            ids: FxHashSet::default(),
        }
    }

    pub(crate) fn resolve(
        &mut self,
        source_id: SourceHandleId,
        limit: usize,
    ) -> XllResult<ResolvedSourceIdentity> {
        if self.ids.contains(&source_id) {
            return Ok(ResolvedSourceIdentity {
                source_id,
                newly_registered: false,
            });
        }

        if self.ids.len() >= limit {
            return Err(XllError::Overloaded);
        }

        self.ids.insert(source_id);

        Ok(ResolvedSourceIdentity {
            source_id,
            newly_registered: true,
        })
    }

    pub(crate) fn rollback_registration(&mut self, identity: ResolvedSourceIdentity) {
        if !identity.newly_registered {
            return;
        }

        self.ids.remove(&identity.source_id);
    }

    pub(crate) fn clear(&mut self) {
        self.ids.clear();
    }
}

pub(crate) static NEXT_RTD_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_runtime_id() -> XllResult<u64> {
    NEXT_RTD_RUNTIME_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::RTD_RT_ID_OVERFLOW,
        })
}

#[derive(Default)]
pub(crate) struct SubscriptionIdentityIndex {
    pub(crate) key_by_identity: FxHashMap<SubscriptionIdentity, SubscriptionKey>,
    pub(crate) identity_by_key: FxHashMap<SubscriptionKey, SubscriptionIdentity>,
}

impl SubscriptionIdentityIndex {
    pub(crate) fn get_key(&self, identity: &SubscriptionIdentity) -> Option<&SubscriptionKey> {
        self.key_by_identity.get(identity)
    }

    pub(crate) fn insert(
        &mut self,
        identity: SubscriptionIdentity,
        key: SubscriptionKey,
    ) -> XllResult<()> {
        if self.key_by_identity.contains_key(&identity) || self.identity_by_key.contains_key(&key) {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::RTD_INDEX_DUPLICATE,
            });
        }

        self.key_by_identity.insert(identity.clone(), key);
        self.identity_by_key.insert(key, identity);

        Ok(())
    }

    pub(crate) fn remove_by_key(&mut self, key: &SubscriptionKey) -> Option<SubscriptionIdentity> {
        let identity = self.identity_by_key.remove(key)?;
        let removed_key = self.key_by_identity.remove(&identity);
        debug_assert_eq!(removed_key.as_ref(), Some(key));
        Some(identity)
    }

    pub(crate) fn clear(&mut self) {
        self.key_by_identity.clear();
        self.identity_by_key.clear();
    }
}
