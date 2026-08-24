use super::source::SourceHandleId;
use super::topic::{SubscriptionIdentity, SubscriptionKey};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub(crate) struct SourceIdentityReservation {
    source_id: SourceHandleId,
}

impl SourceIdentityReservation {
    pub(crate) fn source_id(&self) -> SourceHandleId {
        self.source_id
    }

    pub(crate) fn commit(self) {
        // The registry keeps the committed identity reference. Consuming this
        // token makes that ownership transfer explicit; rollback is performed
        // by `SourceIdentityRegistry::release` before the identity is inserted.
    }
}

// Source handles carry a generation-owned identity. The registry therefore
// stores only that typed identity; it never derives identity from an allocation
// address and does not need an ownership anchor. The reference count tracks
// the number of live subscription identities using each source, so the limit
// is a limit on live distinct sources rather than a lifetime allocation quota.
pub(crate) struct SourceIdentityRegistry {
    pub(crate) refs: FxHashMap<SourceHandleId, NonZeroUsize>,
}

impl SourceIdentityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            refs: FxHashMap::default(),
        }
    }

    pub(crate) fn reserve(
        &mut self,
        source_id: SourceHandleId,
        limit: usize,
    ) -> XllResult<SourceIdentityReservation> {
        if let Some(refs) = self.refs.get_mut(&source_id) {
            let next = refs.get().checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
            })?;
            *refs = NonZeroUsize::new(next).expect("the incremented source refcount is non-zero");
        } else {
            if self.refs.len() >= limit {
                return Err(XllError::Overloaded);
            }

            self.refs
                .insert(source_id, NonZeroUsize::new(1).expect("one is non-zero"));
        }

        Ok(SourceIdentityReservation { source_id })
    }

    pub(crate) fn release(&mut self, reservation: SourceIdentityReservation) {
        self.release_source(reservation.source_id);
    }

    pub(crate) fn release_source(&mut self, source_id: SourceHandleId) {
        let Some(refs) = self.refs.get_mut(&source_id) else {
            debug_assert!(false, "source identity release is balanced");
            return;
        };

        if refs.get() == 1 {
            self.refs.remove(&source_id);
        } else {
            *refs = NonZeroUsize::new(refs.get() - 1)
                .expect("a source refcount greater than one remains non-zero");
        }
    }

    pub(crate) fn clear(&mut self) {
        self.refs.clear();
    }
}

pub(crate) static NEXT_RTD_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_runtime_id() -> XllResult<u64> {
    NEXT_RTD_RUNTIME_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_RT_ID_OVERFLOW,
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
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_DUPLICATE,
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
