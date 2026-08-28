use super::source::SourceHandleId;
use super::topic::{SubscriptionId, SubscriptionIdentity};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

enum SourceRefUpdate {
    Insert,
    Increment(NonZeroUsize),
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
    pub(crate) id_by_identity: FxHashMap<SubscriptionIdentity, SubscriptionId>,
    source_refs: FxHashMap<SourceHandleId, NonZeroUsize>,
}

impl SubscriptionIdentityIndex {
    pub(crate) fn get_id(&self, identity: &SubscriptionIdentity) -> Option<SubscriptionId> {
        self.id_by_identity.get(identity).copied()
    }

    fn plan_insert(
        &self,
        identity: &SubscriptionIdentity,
        _id: SubscriptionId,
        max_source_ids: usize,
    ) -> XllResult<SourceRefUpdate> {
        if self.id_by_identity.contains_key(identity) {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_DUPLICATE,
            });
        }

        let source_id = identity.source_id.0;
        let source_ref_update = match self.source_refs.get(&source_id) {
            Some(current) => {
                let next = current.get().checked_add(1).ok_or(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
                })?;
                SourceRefUpdate::Increment(
                    NonZeroUsize::new(next).expect("the incremented source refcount is non-zero"),
                )
            }
            None => {
                if self.source_refs.len() >= max_source_ids {
                    return Err(XllError::Overloaded);
                }
                SourceRefUpdate::Insert
            }
        };
        Ok(source_ref_update)
    }

    fn commit_insert(
        &mut self,
        identity: SubscriptionIdentity,
        id: SubscriptionId,
        source_ref_update: SourceRefUpdate,
    ) {
        let source_id = identity.source_id.0;
        if self.id_by_identity.insert(identity, id).is_some() {
            xlfn_kernel::invariant::fail_stop();
        }

        match source_ref_update {
            SourceRefUpdate::Insert => {
                if self
                    .source_refs
                    .insert(source_id, NonZeroUsize::new(1).expect("one is non-zero"))
                    .is_some()
                {
                    xlfn_kernel::invariant::fail_stop();
                }
            }
            SourceRefUpdate::Increment(next) => {
                let current = self
                    .source_refs
                    .get_mut(&source_id)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
                *current = next;
            }
        }
    }

    pub(crate) fn insert(
        &mut self,
        identity: SubscriptionIdentity,
        id: SubscriptionId,
        max_source_ids: usize,
    ) -> XllResult<()> {
        let source_ref_update = self.plan_insert(&identity, id, max_source_ids)?;
        self.commit_insert(identity, id, source_ref_update);
        Ok(())
    }

    pub(crate) fn remove(&mut self, identity: &SubscriptionIdentity) -> Option<SubscriptionId> {
        let id = self.id_by_identity.remove(identity)?;
        release_ref(&mut self.source_refs, identity.source_id.0);
        Some(id)
    }

    pub(crate) fn clear(&mut self) {
        self.id_by_identity.clear();
        self.source_refs.clear();
    }

    #[cfg(test)]
    pub(crate) fn source_ref_count(&self, source_id: SourceHandleId) -> Option<NonZeroUsize> {
        self.source_refs.get(&source_id).copied()
    }

    #[cfg(test)]
    pub(crate) fn distinct_source_count(&self) -> usize {
        self.source_refs.len()
    }

    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        let mut expected_source_refs = FxHashMap::default();
        for identity in self.id_by_identity.keys() {
            *expected_source_refs
                .entry(identity.source_id.0)
                .or_insert(0usize) += 1;
        }
        assert_eq!(expected_source_refs.len(), self.source_refs.len());
        for (source_id, refs) in expected_source_refs {
            assert_eq!(
                self.source_refs.get(&source_id).map(|value| value.get()),
                Some(refs)
            );
        }
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
