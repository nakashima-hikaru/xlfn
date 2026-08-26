use super::source::SourceHandleId;
use super::topic::{SubscriptionIdentity, SubscriptionKey};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

#[derive(Debug)]
pub(crate) struct SourceIdentityReservation {
    source_id: SourceHandleId,
    registry: Weak<Mutex<FxHashMap<SourceHandleId, NonZeroUsize>>>,
    committed: bool,
}

impl SourceIdentityReservation {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SourceIdentityReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        release_ref(&mut registry.lock(), self.source_id);
    }
}

// Source handles carry a generation-owned identity. The registry therefore
// stores only that typed identity; it never derives identity from an allocation
// address and does not need an ownership anchor. The reference count tracks
// the number of live subscription identities using each source, so the limit
// is a limit on live distinct sources rather than a lifetime allocation quota.
pub(crate) struct SourceIdentityRegistry {
    refs: Arc<Mutex<FxHashMap<SourceHandleId, NonZeroUsize>>>,
}

impl SourceIdentityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            refs: Arc::new(Mutex::new(FxHashMap::default())),
        }
    }

    pub(crate) fn reserve(
        &mut self,
        source_id: SourceHandleId,
        limit: usize,
    ) -> XllResult<SourceIdentityReservation> {
        let mut refs = self.refs.lock();
        if let Some(refs) = refs.get_mut(&source_id) {
            let next = refs.get().checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
            })?;
            *refs = NonZeroUsize::new(next).expect("the incremented source refcount is non-zero");
        } else {
            if refs.len() >= limit {
                return Err(XllError::Overloaded);
            }

            refs.insert(source_id, NonZeroUsize::new(1).expect("one is non-zero"));
        }

        drop(refs);
        Ok(SourceIdentityReservation {
            source_id,
            registry: Arc::downgrade(&self.refs),
            committed: false,
        })
    }

    pub(crate) fn release_source(&self, source_id: SourceHandleId) {
        release_ref(&mut self.refs.lock(), source_id);
    }

    pub(crate) fn clear(&self) {
        self.refs.lock().clear();
    }

    #[cfg(test)]
    pub(crate) fn distinct_count(&self) -> usize {
        self.refs.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn ref_count(&self, source_id: SourceHandleId) -> Option<NonZeroUsize> {
        self.refs.lock().get(&source_id).copied()
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> FxHashMap<SourceHandleId, NonZeroUsize> {
        self.refs.lock().clone()
    }
}

fn release_ref(refs: &mut FxHashMap<SourceHandleId, NonZeroUsize>, source_id: SourceHandleId) {
    let count = refs
        .get_mut(&source_id)
        .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());

    if count.get() == 1 {
        refs.remove(&source_id);
    } else {
        *count = NonZeroUsize::new(count.get() - 1)
            .expect("a source refcount greater than one remains non-zero");
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
