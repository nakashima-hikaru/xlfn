use super::source::SourceHandleId;
use super::topic::{SubscriptionId, SubscriptionIdentityKey};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

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
    pub(crate) ids_by_key: FxHashMap<SubscriptionIdentityKey, SmallVec<[SubscriptionId; 1]>>,
    source_refs: FxHashMap<SourceHandleId, NonZeroUsize>,
}

impl SubscriptionIdentityIndex {
    pub(crate) fn candidates(&self, key: SubscriptionIdentityKey) -> &[SubscriptionId] {
        self.ids_by_key.get(&key).map_or(&[], SmallVec::as_slice)
    }

    pub(super) fn prepare_insert(
        &mut self,
        key: SubscriptionIdentityKey,
        id: SubscriptionId,
        max_source_ids: usize,
    ) -> XllResult<IdentityInsertion<'_>> {
        let source_count = self.source_refs.len();
        let bucket = self.ids_by_key.entry(key);
        if let std::collections::hash_map::Entry::Occupied(existing) = &bucket
            && existing.get().contains(&id)
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_DUPLICATE,
            });
        }
        let source = match self.source_refs.entry(key.source_id.0) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let next = entry.get().checked_add(1).ok_or(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
                })?;
                SourceInsertion::Increment { entry, next }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                if source_count >= max_source_ids {
                    return Err(XllError::Overloaded);
                }
                SourceInsertion::New(entry)
            }
        };
        Ok(IdentityInsertion { bucket, source, id })
    }

    #[cfg(test)]
    pub(crate) fn insert(
        &mut self,
        key: SubscriptionIdentityKey,
        id: SubscriptionId,
        max_source_ids: usize,
    ) -> XllResult<()> {
        self.prepare_insert(key, id, max_source_ids)?.commit();
        Ok(())
    }

    pub(crate) fn remove(&mut self, key: SubscriptionIdentityKey, id: SubscriptionId) -> bool {
        let Some(ids) = self.ids_by_key.get_mut(&key) else {
            return false;
        };
        let Some(position) = ids.iter().position(|&candidate| candidate == id) else {
            return false;
        };
        ids.swap_remove(position);
        if ids.is_empty() {
            self.ids_by_key.remove(&key);
        }
        release_ref(&mut self.source_refs, key.source_id.0);
        true
    }

    pub(crate) fn clear(&mut self) {
        self.ids_by_key.clear();
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
        for (key, ids) in &self.ids_by_key {
            assert!(!ids.is_empty());
            *expected_source_refs
                .entry(key.source_id.0)
                .or_insert(0usize) += ids.len();
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

enum SourceInsertion<'index> {
    New(std::collections::hash_map::VacantEntry<'index, SourceHandleId, NonZeroUsize>),
    Increment {
        entry: std::collections::hash_map::OccupiedEntry<'index, SourceHandleId, NonZeroUsize>,
        next: NonZeroUsize,
    },
}

/// Native entry borrows retain both exact maps through commit. Callers cannot
/// substitute a source, reuse an increment plan or commit against another index.
pub(super) struct IdentityInsertion<'index> {
    bucket: std::collections::hash_map::Entry<
        'index,
        SubscriptionIdentityKey,
        SmallVec<[SubscriptionId; 1]>,
    >,
    source: SourceInsertion<'index>,
    id: SubscriptionId,
}
impl IdentityInsertion<'_> {
    pub(super) fn commit(self) {
        self.bucket.or_default().push(self.id);
        match self.source {
            SourceInsertion::New(entry) => {
                entry.insert(NonZeroUsize::MIN);
            }
            SourceInsertion::Increment { mut entry, next } => {
                *entry.get_mut() = next;
            }
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
